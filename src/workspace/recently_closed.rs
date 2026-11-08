//! App-wide history of confirmed user closes. A history entry opens fresh sessions.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;

use async_channel::Receiver;
use iced::widget::{button, column, container, text};
use iced::{Element, Length, Task, window};
use kortty_terminal::TerminalProfile;

use crate::connection_store::Store;
use crate::profile::session::{
    ClosedEntry, ClosedTab, MAX_CLOSED_ENTRIES, MAX_CLOSED_TABS, RecentlyClosed,
};
use crate::session::{IbmKind, SerialConnection, Session, SessionId};
use crate::session_security::LocalSettings;

use super::ssh_access::ReopenSshOrigin;
use super::{Message as Desktop, PaneSession, PendingIbm, Tab, Window, Workspace, initial_grid};

#[derive(Clone)]
pub(super) enum Origin {
    Local(TerminalProfile, Option<u8>, LocalSettings),
    Ssh(Box<ReopenSshOrigin>),
    Saved(u64),
    Serial(TerminalProfile, Option<u8>, Arc<SerialConnection>),
    Ibm(IbmOrigin, Option<u8>),
}

#[derive(Clone)]
pub(super) struct IbmOrigin {
    pub(super) kind: IbmKind,
    pub(super) address: Option<SocketAddr>,
    pub(super) device_name: String,
}

impl Origin {
    pub(super) fn capture(pane: &PaneSession) -> Option<Self> {
        let profile = pane.frame.screen.profile;
        let font = pane.font_size_override();
        if let Some(ssh) = ReopenSshOrigin::from_pane(pane) {
            return Some(Self::Ssh(Box::new(ssh)));
        }
        if let Some(serial) = &pane.frame.serial {
            return Some(Self::Serial(profile, font, Arc::clone(serial)));
        }
        if let Some(ibm) = &pane.frame.ibm {
            let origin = pane.ibm_origin.clone().unwrap_or(IbmOrigin {
                kind: ibm.kind,
                address: None,
                device_name: String::new(),
            });
            return Some(Self::Ibm(origin, font));
        }
        Some(Self::Local(profile, font, pane.local_security.clone()?))
    }

    fn saved_id(&self) -> Option<u64> {
        match self {
            Self::Ssh(ssh) => ssh.saved_id(),
            Self::Saved(id) => Some(*id),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub(super) struct Remembered {
    origin: Origin,
    custom_title: Option<String>,
    group: Option<String>,
    label: String,
    serial_shutdown: Vec<Receiver<()>>,
}

struct Entry {
    id: u64,
    window: bool,
    tabs: Vec<Remembered>,
}

impl Entry {
    fn label(&self) -> String {
        let mut names = self
            .tabs
            .iter()
            .take(3)
            .map(|tab| tab.label.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        if self.tabs.len() > 3 {
            names.push_str(&format!(", +{}", self.tabs.len() - 3));
        }
        if self.window {
            format!("Window: {names}")
        } else {
            names
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReopenToken(u64);

#[derive(Clone, Copy, Debug)]
pub(super) enum SshOutcome {
    Opened(u64),
    Canceled,
    Unavailable,
}

#[derive(Clone, Debug)]
pub(crate) enum Message {
    ReopenLatest(window::Id),
    Advance(window::Id, u64),
    SerialReleased(window::Id, ReopenToken),
    Cancel(window::Id, ReopenToken),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Ready,
    Ssh(ReopenToken),
    Serial(ReopenToken),
    Ibm(ReopenToken),
}

struct Flight {
    entry: u64,
    owner: window::Id,
    created: bool,
    index: usize,
    phase: Phase,
}

#[derive(Default)]
pub(super) struct History {
    entries: VecDeque<Entry>,
    flight: Option<Flight>,
}

impl Workspace {
    pub(super) fn load_recently_closed(&mut self, stored: RecentlyClosed) {
        if stored.entries.is_empty() {
            return;
        }
        let store = match crate::profile::connections_path().and_then(Store::open) {
            Ok(store) => store,
            Err(error) => {
                self.session_restore.history_load_failed(error);
                return;
            }
        };
        for entry in stored.entries {
            let tabs = entry
                .tabs
                .into_iter()
                .filter_map(|tab| {
                    let connection = store
                        .get(tab.saved_id)
                        .filter(|connection| !connection.incognito)?;
                    let label =
                        history_label(tab.custom_title.as_deref().unwrap_or(&connection.name));
                    Some(Remembered {
                        origin: Origin::Saved(tab.saved_id),
                        label,
                        custom_title: tab.custom_title,
                        group: tab.group,
                        serial_shutdown: Vec::new(),
                    })
                })
                .collect::<Vec<_>>();
            if !tabs.is_empty() {
                let id = self.allocate();
                self.recently_closed.entries.push_back(Entry {
                    id,
                    window: entry.window,
                    tabs,
                });
            }
        }
    }

    pub(super) fn remember_tab(&self, tab: &Tab) -> Option<Remembered> {
        if tab.incognito {
            return None;
        }
        let origin = tab.reopen_origin.clone()?;
        let name = tab
            .custom_title
            .as_deref()
            .map(history_label)
            .unwrap_or_else(|| {
                let name = match &origin {
                    Origin::Local(_, _, settings) => settings
                        .shell
                        .preset_title()
                        .unwrap_or("Local Shell")
                        .into(),
                    Origin::Ssh(ssh) => ssh.label(),
                    Origin::Serial(_, _, connection) => connection.device.display().to_string(),
                    Origin::Ibm(origin, _) => origin.kind.label().into(),
                    Origin::Saved(_) => "SSH".into(),
                };
                history_label(&name)
            });
        let serial_shutdown = if matches!(origin, Origin::Serial(..)) {
            tab.grid
                .iter()
                .filter_map(|(_, id)| {
                    self.sessions
                        .get(id)
                        .filter(|pane| pane.frame.serial.is_some())
                })
                .map(|pane| pane.session.shutdown_handle())
                .collect()
        } else {
            Vec::new()
        };
        Some(Remembered {
            origin,
            custom_title: tab.custom_title.clone(),
            group: tab.group.clone(),
            label: name,
            serial_shutdown,
        })
    }

    pub(super) fn remember_closed_tabs(&mut self, closed_window: bool, tabs: Vec<Remembered>) {
        if tabs.is_empty() {
            return;
        }
        let id = self.allocate();
        self.recently_closed.entries.push_front(Entry {
            id,
            window: closed_window,
            tabs,
        });
        if self.recently_closed.entries.len() > MAX_CLOSED_ENTRIES {
            let reserved = self
                .recently_closed
                .flight
                .as_ref()
                .map(|flight| flight.entry);
            if let Some(index) = self
                .recently_closed
                .entries
                .iter()
                .rposition(|entry| Some(entry.id) != reserved)
            {
                self.recently_closed.entries.remove(index);
            }
        }
        self.persist_recently_closed();
    }

    fn persist_recently_closed(&self) {
        let entries = self
            .recently_closed
            .entries
            .iter()
            .filter_map(|entry| {
                let tabs = entry
                    .tabs
                    .iter()
                    .filter_map(|tab| {
                        Some(ClosedTab {
                            saved_id: tab.origin.saved_id()?,
                            custom_title: tab.custom_title.clone(),
                            group: tab.group.clone(),
                        })
                    })
                    .take(MAX_CLOSED_TABS)
                    .collect::<Vec<_>>();
                (!tabs.is_empty()).then_some(ClosedEntry {
                    window: entry.window,
                    tabs,
                })
            })
            .collect();
        self.session_restore
            .save_recently_closed(RecentlyClosed { entries });
    }

    pub(super) fn has_recently_closed(&self) -> bool {
        self.recently_closed.flight.is_none() && self.has_recently_closed_entries()
    }

    pub(super) fn has_recently_closed_entries(&self) -> bool {
        !self.recently_closed.entries.is_empty()
    }

    pub(super) fn recently_closed_menu_rows(&self) -> Vec<(u64, String)> {
        self.recently_closed
            .entries
            .iter()
            .filter(|entry| {
                !entry.tabs.is_empty()
                    && self
                        .recently_closed
                        .flight
                        .as_ref()
                        .is_none_or(|flight| flight.entry != entry.id)
            })
            .map(|entry| (entry.id, entry.label()))
            .collect()
    }

    pub(super) fn reopen_closed_tab(
        &mut self,
        owner: Option<window::Id>,
        entry: Option<u64>,
    ) -> Task<Desktop> {
        if self.recently_closed.flight.is_some() || self.clipboard.policy.problem.is_some() {
            return Task::none();
        }
        let selected = match entry {
            Some(id) => self
                .recently_closed
                .entries
                .iter()
                .find(|entry| entry.id == id),
            None => self.recently_closed.entries.front(),
        };
        let owner = owner.or_else(|| {
            self.windows
                .iter()
                .max_by_key(|(_, window)| (window.focused, window.number))
                .map(|(&id, _)| id)
        });
        let Some(selected) = selected else {
            if entry.is_none()
                && let Some(owner) = owner
            {
                self.notice(owner, "No closed tab to reopen".into());
            }
            return Task::none();
        };
        if owner.is_some_and(|id| !self.palette_can_open(id)) {
            return Task::none();
        }
        let entry = selected.id;
        let existing = owner.filter(|id| {
            self.windows
                .get(id)
                .is_some_and(|window| !selected.window || window.tabs.is_empty())
        });
        let (owner, created, open) = if let Some(owner) = existing {
            (owner, false, Task::none())
        } else {
            let (owner, opened) = self.open_window(false);
            (owner, true, opened)
        };
        self.recently_closed.flight = Some(Flight {
            entry,
            owner,
            created,
            index: 0,
            phase: Phase::Ready,
        });
        open.chain(self.reopen_next(owner))
    }

    pub(super) fn clear_recently_closed(&mut self) -> Task<Desktop> {
        self.recently_closed.entries.clear();
        self.persist_recently_closed();
        let Some(owner) = self
            .recently_closed
            .flight
            .as_ref()
            .map(|flight| flight.owner)
        else {
            return Task::none();
        };
        let created = self.cancel_reopen_owner(owner);
        self.close_empty_reopen_window(owner, created)
    }

    pub(super) fn cancel_reopen_owner(&mut self, owner: window::Id) -> bool {
        let Some(flight) = self
            .recently_closed
            .flight
            .take_if(|flight| flight.owner == owner)
        else {
            return false;
        };
        match flight.phase {
            Phase::Ssh(token) => self.discard_reopen_ssh(owner, token),
            Phase::Ibm(_) => {
                if let Some(window) = self.windows.get_mut(&owner) {
                    window.pending_ibm = None;
                }
            }
            _ => {}
        }
        flight.created
    }

    fn reopen_next(&mut self, owner: window::Id) -> Task<Desktop> {
        let Some(flight) = self
            .recently_closed
            .flight
            .as_ref()
            .filter(|flight| flight.owner == owner && flight.phase == Phase::Ready)
        else {
            return Task::none();
        };
        let member = self
            .recently_closed
            .entries
            .iter()
            .find(|entry| entry.id == flight.entry)
            .and_then(|entry| entry.tabs.get(flight.index))
            .cloned();
        let Some(member) = member else {
            let created = flight.created;
            self.recently_closed.flight = None;
            self.recently_closed
                .entries
                .retain(|entry| !entry.tabs.is_empty());
            return self.close_empty_reopen_window(owner, created);
        };
        let token = ReopenToken(self.allocate());
        let phase = match &member.origin {
            Origin::Ssh(_) | Origin::Saved(_) => Phase::Ssh(token),
            Origin::Ibm(..) => Phase::Ibm(token),
            _ => Phase::Serial(token),
        };
        if let Some(flight) = self.recently_closed.flight.as_mut() {
            flight.phase = phase;
        }
        match member.origin {
            Origin::Ssh(origin) => self.reopen_ssh(owner, token, &origin),
            Origin::Saved(id) => self.reopen_saved_ssh(owner, token, id),
            Origin::Local(profile, font, settings) => {
                let session = self.store_local_session(profile, font, Some(settings));
                self.attach_reopened_session(owner, token, session)
            }
            Origin::Serial(..) => Task::perform(
                async move {
                    for stopped in member.serial_shutdown {
                        let _ = stopped.recv().await;
                    }
                },
                move |()| Desktop::RecentlyClosed(Message::SerialReleased(owner, token)),
            ),
            Origin::Ibm(origin, font) => {
                if !self.allow_in_process_transport(owner, "IBM") {
                    return self.finish_reopen_member(owner, phase, SshOutcome::Unavailable);
                }
                let Some(window) = self.windows.get_mut(&owner) else {
                    return self.finish_reopen_member(owner, phase, SshOutcome::Canceled);
                };
                window.pending_ibm = Some(PendingIbm {
                    kind: origin.kind,
                    replace: None,
                    reconnect: None,
                    font,
                    address: origin
                        .address
                        .map_or_else(String::new, |address| address.to_string()),
                    device_name: origin.device_name,
                    plaintext: false,
                });
                Task::none()
            }
        }
    }

    pub(super) fn update_recently_closed(&mut self, message: Message) -> Task<Desktop> {
        let (owner, token) = match message {
            Message::ReopenLatest(owner) => return self.reopen_closed_tab(Some(owner), None),
            Message::Advance(owner, entry) => {
                if self
                    .recently_closed
                    .flight
                    .as_ref()
                    .is_some_and(|flight| flight.entry == entry)
                {
                    return self.reopen_next(owner);
                }
                return Task::none();
            }
            Message::Cancel(owner, token) => {
                return self.finish_reopen_member(
                    owner,
                    Phase::Serial(token),
                    SshOutcome::Canceled,
                );
            }
            Message::SerialReleased(owner, token) => (owner, token),
        };
        let Some(flight) = self
            .recently_closed
            .flight
            .as_ref()
            .filter(|flight| flight.owner == owner && flight.phase == Phase::Serial(token))
        else {
            return Task::none();
        };
        let member = self
            .recently_closed
            .entries
            .iter()
            .find(|entry| entry.id == flight.entry)
            .and_then(|entry| entry.tabs.get(flight.index));
        let Some(Origin::Serial(profile, font, connection)) =
            member.map(|member| &member.origin).cloned()
        else {
            return Task::none();
        };
        if !self.clipboard.policy.serial_allowed
            || !self.allow_in_process_transport(owner, "Serial connections")
        {
            return self.finish_reopen_member(owner, Phase::Serial(token), SshOutcome::Unavailable);
        }
        let grid = initial_grid(profile);
        let session = Session::start_serial(
            (*connection).clone(),
            grid,
            profile,
            self.window_settings.prompt_navigation_enabled,
            self.window_settings.terminal_appearance(),
        )
        .map(|session| self.store_connected_session(session, grid, None, font));
        self.attach_reopened_session(owner, token, session)
    }

    fn attach_reopened_session(
        &mut self,
        owner: window::Id,
        token: ReopenToken,
        session: std::io::Result<SessionId>,
    ) -> Task<Desktop> {
        let outcome = match session {
            Ok(session) => {
                self.add_tab(owner, session);
                self.windows
                    .get(&owner)
                    .and_then(Window::tab)
                    .map_or(SshOutcome::Canceled, |tab| SshOutcome::Opened(tab.id))
            }
            Err(error) => {
                self.notice(owner, format!("Could not reopen the terminal ({error})"));
                SshOutcome::Unavailable
            }
        };
        self.finish_reopen_member(owner, Phase::Serial(token), outcome)
    }

    pub(super) fn finish_reopen_ssh(
        &mut self,
        owner: window::Id,
        token: ReopenToken,
        outcome: SshOutcome,
    ) -> Task<Desktop> {
        self.finish_reopen_member(owner, Phase::Ssh(token), outcome)
    }

    pub(super) fn finish_reopen_ibm(
        &mut self,
        owner: window::Id,
        outcome: SshOutcome,
    ) -> Task<Desktop> {
        match self
            .recently_closed
            .flight
            .as_ref()
            .filter(|flight| flight.owner == owner)
            .map(|flight| flight.phase)
        {
            Some(phase @ Phase::Ibm(_)) => self.finish_reopen_member(owner, phase, outcome),
            _ => Task::none(),
        }
    }

    fn finish_reopen_member(
        &mut self,
        owner: window::Id,
        phase: Phase,
        outcome: SshOutcome,
    ) -> Task<Desktop> {
        let Some(flight) = self
            .recently_closed
            .flight
            .as_mut()
            .filter(|flight| flight.owner == owner && flight.phase == phase)
        else {
            return Task::none();
        };
        let Some(entry) = self
            .recently_closed
            .entries
            .iter_mut()
            .find(|entry| entry.id == flight.entry)
        else {
            return Task::none();
        };
        if matches!(outcome, SshOutcome::Canceled) {
            flight.index += 1;
        } else if flight.index < entry.tabs.len() {
            let closed = entry.tabs.remove(flight.index);
            if let SshOutcome::Opened(id) = outcome
                && let Some(window) = self.windows.get_mut(&owner)
            {
                if let Some(tab) = window.terminal_tabs_mut().find(|tab| tab.id == id) {
                    tab.custom_title = closed.custom_title;
                    tab.group = closed.group;
                }
                window.arrange_tab_groups();
            }
        }
        flight.phase = Phase::Ready;
        let entry = flight.entry;
        self.persist_recently_closed();
        self.session_restore.changed(false);
        Task::done(Desktop::RecentlyClosed(Message::Advance(owner, entry)))
    }

    fn close_empty_reopen_window(&mut self, owner: window::Id, created: bool) -> Task<Desktop> {
        if created
            && self
                .windows
                .get(&owner)
                .is_some_and(|window| window.tabs.is_empty())
        {
            return self.close_window(owner);
        }
        Task::none()
    }

    pub(super) fn reopen_waiting_for_serial(&self, owner: window::Id) -> bool {
        self.recently_closed
            .flight
            .as_ref()
            .is_some_and(|flight| flight.owner == owner && matches!(flight.phase, Phase::Serial(_)))
    }

    pub(super) fn reopen_wait_view(&self, owner: window::Id) -> Option<Element<'_, Desktop>> {
        let flight = self
            .recently_closed
            .flight
            .as_ref()
            .filter(|flight| flight.owner == owner)?;
        let Phase::Serial(token) = flight.phase else {
            return None;
        };
        Some(
            container(
                column![
                    text("Waiting for the serial port to close…").size(18),
                    button("Cancel")
                        .on_press(Desktop::RecentlyClosed(Message::Cancel(owner, token))),
                ]
                .spacing(12),
            )
            .center(Length::Fill)
            .into(),
        )
    }

    pub(super) fn reopen_wait_cancel(&mut self, owner: window::Id) -> Option<Task<Desktop>> {
        let flight = self
            .recently_closed
            .flight
            .as_ref()
            .filter(|flight| flight.owner == owner)?;
        let Phase::Serial(token) = flight.phase else {
            return None;
        };
        Some(self.update_recently_closed(Message::Cancel(owner, token)))
    }

    pub(super) fn reconcile_reopen_prompt(&mut self) -> Option<Task<Desktop>> {
        let flight = self.recently_closed.flight.as_ref()?;
        let (owner, phase) = (flight.owner, flight.phase);
        let window = self.windows.get(&owner)?;
        let missing = match phase {
            Phase::Ibm(_) => window.pending_ibm.is_none(),
            Phase::Ssh(token) => {
                window
                    .pending_ssh
                    .as_ref()
                    .and_then(|pending| pending.reopen_token())
                    != Some(token)
            }
            _ => false,
        };
        missing.then(|| self.finish_reopen_member(owner, phase, SshOutcome::Canceled))
    }

    pub(super) fn reopen_busy(&self, owner: window::Id) -> bool {
        self.recently_closed
            .flight
            .as_ref()
            .is_some_and(|flight| flight.owner == owner)
    }
}

fn history_label(label: &str) -> String {
    let label = crate::profile::normalize_tab_title(label);
    let mut characters = label.chars();
    let mut short: String = characters.by_ref().take(60).collect();
    if characters.next().is_some() {
        short.pop();
        short.push('…');
    }
    short
}

#[cfg(test)]
mod tests;
