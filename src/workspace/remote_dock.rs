use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use async_channel::Receiver;
use iced::futures::{StreamExt, stream::BoxStream};
use iced::keyboard::{Key, Modifiers, key::Named};
use iced::{Subscription, Task, window};

use crate::session::{SessionId, Status};
use crate::session_security::IsolationLevel;
use crate::sftp_window::{ConflictChoice, EntryKind, EntryName};
use crate::ssh_connection::SftpOpener;

use super::tab_titles::{
    REMOTE_SIDEBAR_MAX_WIDTH, REMOTE_SIDEBAR_MIN_WIDTH, RemoteSidebarPosition,
};
use super::{Message as Desktop, PaneSession, Tab, Window, Workspace};

mod dialog;
mod follow;
mod model;
mod view;
pub(super) use dialog::{Dialogs, Message as DialogMessage};
use follow::Follow;
use model::{Browser, Completion};

const PAGE: usize = 120;
const MAX_SELECTION: usize = 256;
const MAX_DROP_FILES: usize = 256;

#[derive(Default)]
pub(super) struct Dock {
    binding: Option<Binding>,
    filter: String,
    selected: HashSet<EntryName>,
    anchor: Option<EntryName>,
    page: usize,
    keyboard_focus: bool,
    pending_drop: Option<PendingDrop>,
}

struct Binding {
    generation: u64,
    browser: Browser,
    follow: Follow,
    listing: u64,
    manager_loan: Option<ManagerLoan>,
}

enum ManagerLoan {
    Opening(Receiver<()>),
    Manager,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Target {
    tab: u64,
    generation: u64,
    session: SessionId,
    path: Vec<u8>,
    listing: u64,
}

struct PendingDrop {
    target: Target,
    paths: Vec<PathBuf>,
    ready: Instant,
}

#[derive(Clone, Debug)]
pub enum Message {
    Action(u64, Action),
    Completed(u64, u64, Completion),
    Resize(RemoteSidebarPosition, f32),
    FinishResize(RemoteSidebarPosition),
    Modifiers(Modifiers),
    ManagerReleased(u64, u64, Option<Vec<u8>>),
    ManagerReturned(SessionId),
    ManagerAfterBrowser(SessionId, Option<Vec<u8>>),
    Dialog(dialog::Message),
}

#[derive(Clone, Debug)]
pub enum Action {
    Hide,
    Pin,
    Filter(String),
    Refresh,
    Page(bool),
    Select(u64, u64, EntryName),
    Open(u64, u64, EntryName),
    Browse(u64, u64, Vec<u8>),
    OpenManager(u64, u64),
    Upload(u64, u64),
    Download(u64, u64),
    Dropped(u64, u64, PathBuf),
    Resolve(u64, u64, ConflictChoice),
}

impl Dock {
    pub(super) fn release_keyboard(&mut self) {
        self.keyboard_focus = false;
    }

    fn reset_selection(&mut self) {
        self.selected.clear();
        self.anchor = None;
        self.page = 0;
    }

    fn select(&mut self, name: EntryName, modifiers: Modifiers) {
        let Some(binding) = &self.binding else { return };
        let entries = binding.browser.entries();
        let Some(index) = entries.iter().position(|entry| entry.name == name) else {
            return;
        };
        self.keyboard_focus = true;
        if modifiers.shift()
            && let Some(anchor) = &self.anchor
            && let Some(start) = entries.iter().position(|entry| &entry.name == anchor)
        {
            if !modifiers.command() {
                self.selected.clear();
            }
            let filter = self.filter.trim().to_lowercase();
            for entry in &entries[start.min(index)..=start.max(index)] {
                if self.selected.len() >= MAX_SELECTION {
                    break;
                }
                if filter.is_empty() || entry.display.to_lowercase().contains(&filter) {
                    self.selected.insert(entry.name.clone());
                }
            }
        } else {
            if !modifiers.command() {
                self.selected.clear();
            }
            if modifiers.command() && self.selected.contains(&name) {
                self.selected.remove(&name);
            } else if self.selected.len() < MAX_SELECTION {
                self.selected.insert(name.clone());
            }
            self.anchor = Some(name);
        }
    }
}

impl Binding {
    fn target(&self, tab: u64) -> Option<Target> {
        Some(Target {
            tab,
            generation: self.generation,
            session: self.browser.session(),
            path: self.browser.path()?.to_vec(),
            listing: self.listing,
        })
    }

    fn action_matches(&self, generation: u64, listing: u64) -> bool {
        self.generation == generation
            && self.listing == listing
            && self.browser.connected()
            && self.manager_loan.is_none()
    }
}

impl Workspace {
    pub(super) fn remote_dock_key(
        &mut self,
        owner: window::Id,
        key: &Key,
        modifiers: Modifiers,
    ) -> Option<Task<Desktop>> {
        if self.window_settings.remote_sidebar_position == RemoteSidebarPosition::Hidden
            || self.terminal_only_fullscreen_active(owner)
            || modifiers.alt()
            || modifiers.command()
            || modifiers.control()
        {
            return None;
        }
        let window = self.windows.get(&owner)?;
        let tab = window.tab()?;
        let dock = &tab.remote_files;
        let binding = dock
            .binding
            .as_ref()
            .filter(|binding| binding.browser.connected() && dock.keyboard_focus)?;
        let action = match key {
            Key::Named(Named::Escape) => {
                self.active_tab_mut(owner)?.remote_files.release_keyboard();
                return Some(Task::none());
            }
            Key::Named(Named::Enter) if modifiers.is_empty() => {
                Action::Open(binding.generation, binding.listing, dock.anchor.clone()?)
            }
            Key::Named(Named::ArrowUp | Named::ArrowDown) => {
                let filter = dock.filter.trim().to_lowercase();
                let entries = binding
                    .browser
                    .entries()
                    .iter()
                    .filter(|entry| {
                        filter.is_empty() || entry.display.to_lowercase().contains(&filter)
                    })
                    .collect::<Vec<_>>();
                if entries.is_empty() {
                    return Some(Task::none());
                }
                let backwards = *key == Key::Named(Named::ArrowUp);
                let current = dock
                    .anchor
                    .as_ref()
                    .and_then(|anchor| entries.iter().position(|entry| &entry.name == anchor));
                let next = match (current, backwards) {
                    (Some(index), true) => index.saturating_sub(1),
                    (Some(index), false) => (index + 1).min(entries.len() - 1),
                    (None, _) => dock.page * PAGE,
                }
                .min(entries.len() - 1);
                let action = Action::Select(
                    binding.generation,
                    binding.listing,
                    entries[next].name.clone(),
                );
                let task = self.remote_dock_action(owner, tab.id, action);
                if let Some(tab) = self.active_tab_mut(owner) {
                    tab.remote_files.page = next / PAGE;
                }
                return Some(task);
            }
            _ => return None,
        };
        Some(self.remote_dock_action(owner, tab.id, action))
    }

    pub(super) fn remote_dock_subscription(&self) -> Subscription<Desktop> {
        if self.window_settings.remote_sidebar_position == RemoteSidebarPosition::Hidden {
            return Subscription::none();
        }
        if !self.remote_browser_closing.is_empty()
            || self.windows.values().filter_map(Window::tab).any(|tab| {
                tab.remote_files.pending_drop.is_some()
                    || tab.remote_files.binding.as_ref().is_some_and(|binding| {
                        binding.follow.awaiting_listing() || binding.browser.transfers_active()
                    })
            })
        {
            Subscription::run(dock_clock)
        } else {
            Subscription::none()
        }
    }

    pub(super) fn remote_pane(&self, session: SessionId) -> Option<(&PaneSession, SftpOpener)> {
        let pane = self.sessions.get(&session)?;
        let minimum = self.clipboard.policy.minimum_isolation;
        let target = pane.ssh.as_ref()?;
        if self.clipboard.policy.problem.is_some()
            || pane.frame.status != Status::Running
            || target.isolation < minimum
            || (minimum == IsolationLevel::Sandbox && pane.session.sandbox_backend().is_none())
        {
            return None;
        }
        Some((pane, pane.session.sftp_opener()?))
    }

    pub(super) fn tracked_remote_directory(
        &self,
        owner: window::Id,
        session: SessionId,
    ) -> Option<Vec<u8>> {
        let tab = self.windows.get(&owner)?.tab()?;
        let pane = self.sessions.get(&session)?;
        let ssh = pane.ssh.as_ref()?;
        let frame = pane.session.frame();
        if frame.scroll_offset != 0 || frame.screen.alternate_screen {
            return None;
        }
        let mut follow = tab
            .remote_files
            .binding
            .as_ref()
            .filter(|binding| binding.browser.session() == session)
            .map_or_else(Follow::default, |binding| binding.follow.clone());
        follow.observe(
            frame.shell_activity(),
            &frame.screen,
            ssh.username(),
            ssh.host(),
            Instant::now(),
        );
        follow.shell_path().map(<[u8]>::to_vec)
    }

    pub(super) fn remote_shell_file_context(&self, session: SessionId) -> Option<Option<Vec<u8>>> {
        let pane = self.sessions.get(&session)?;
        let ssh = pane.ssh.as_ref()?;
        let frame = pane.session.frame();
        if frame.scroll_offset != 0 || frame.screen.alternate_screen {
            return None;
        }
        follow::file_context(
            frame.shell_activity(),
            &frame.screen,
            ssh.username(),
            ssh.host(),
        )
    }

    pub(super) fn remote_dock_editor_worker(
        &self,
        owner: window::Id,
        session: SessionId,
    ) -> Option<Option<&crate::sftp_window::worker::Worker>> {
        let binding = self
            .windows
            .get(&owner)?
            .tab()?
            .remote_files
            .binding
            .as_ref()?;
        (binding.browser.session() == session
            && (binding.browser.has_worker()
                || matches!(binding.manager_loan, Some(ManagerLoan::Opening(_)))))
        .then(|| binding.browser.terminal_editor_worker())
    }

    fn preferred_remote_pane(&self, tab: &Tab) -> Option<SessionId> {
        let current = tab
            .remote_files
            .binding
            .as_ref()
            .filter(|binding| self.remote_pane(binding.browser.session()).is_some());
        if let Some(current) = current
            && (current.follow.pinned || current.browser.transfers_active())
        {
            return Some(current.browser.session());
        }
        tab.focused_session()
            .filter(|session| self.remote_pane(*session).is_some())
            .or_else(|| current.map(|binding| binding.browser.session()))
            .or_else(|| {
                tab.grid
                    .get(tab.primary)
                    .copied()
                    .filter(|id| self.remote_pane(*id).is_some())
            })
            .or_else(|| {
                tab.grid
                    .iter()
                    .map(|(_, id)| *id)
                    .filter(|id| self.remote_pane(*id).is_some())
                    .min_by_key(|id| id.0)
            })
    }

    pub(super) fn remote_sidebar_preferences_changed(&mut self) -> Task<Desktop> {
        if self.window_settings.remote_sidebar_position == RemoteSidebarPosition::Hidden {
            self.stop_all_remote_docks();
            return self.close_remote_transfer_dialogs();
        }
        self.sync_remote_docks()
    }

    pub(super) fn sync_remote_docks(&mut self) -> Task<Desktop> {
        if self.window_settings.remote_sidebar_position == RemoteSidebarPosition::Hidden {
            return Task::none();
        }
        let ids = self.windows.keys().copied().collect::<Vec<_>>();
        Task::batch(ids.into_iter().map(|owner| self.sync_remote_dock(owner)))
    }

    pub(super) fn remote_dock_session_updated(&mut self, session: SessionId) -> Task<Desktop> {
        if self.window_settings.remote_sidebar_position == RemoteSidebarPosition::Hidden {
            return Task::none();
        }
        let owner = self.windows.iter().find_map(|(&owner, window)| {
            let tab = window.tab()?;
            tab.grid
                .iter()
                .any(|(_, id)| *id == session)
                .then_some(owner)
        });
        owner.map_or_else(Task::none, |owner| self.sync_remote_dock(owner))
    }

    pub(super) fn sync_remote_dock(&mut self, owner: window::Id) -> Task<Desktop> {
        if self.window_settings.remote_sidebar_position == RemoteSidebarPosition::Hidden {
            return Task::none();
        }
        let Some(tab) = self.windows.get(&owner).and_then(Window::tab) else {
            return Task::none();
        };
        if self.terminal_only_fullscreen_active(owner) {
            return Task::none();
        }
        let tab_id = tab.id;
        let current = tab
            .remote_files
            .binding
            .as_ref()
            .map(|binding| binding.browser.session());
        let target = self.preferred_remote_pane(tab);
        let Some(target) = target else {
            if let Some(binding) = self
                .active_tab_mut(owner)
                .and_then(|tab| tab.remote_files.binding.take())
            {
                self.stop_remote_browser(binding.browser);
            }
            return Task::none();
        };
        if self.terminal_file_request_pending(target) {
            return Task::none();
        }
        if let Some(done) = self.remote_browser_closing.get(&target) {
            if !done.is_closed() {
                return Task::none();
            }
            self.remote_browser_closing.remove(&target);
        }
        let Some((pane, opener)) = self.remote_pane(target) else {
            return Task::none();
        };
        let managed = self.remote_manager_owns(target);
        let editor_policy = self.clipboard.policy.remote_editor;
        let ssh = pane.ssh.as_ref().expect("validated SSH pane");
        let (user, host) = (ssh.username().to_owned(), ssh.host().to_owned());
        let frame = pane.session.frame();
        let activity = frame.shell_activity();
        let now = Instant::now();
        if current == Some(target) {
            let resuming = !managed
                && tab.remote_files.binding.as_ref().is_some_and(|binding| {
                    matches!(binding.manager_loan, Some(ManagerLoan::Manager))
                });
            let worker = resuming
                .then(|| self.take_terminal_file_worker(target))
                .flatten();
            let dock = &mut self
                .active_tab_mut(owner)
                .expect("visible tab")
                .remote_files;
            let binding = dock.binding.as_mut().expect("current remote binding");
            if frame.scroll_offset == 0 {
                binding
                    .follow
                    .observe(activity, &frame.screen, &user, &host, now);
            }
            if resuming {
                binding.manager_loan = None;
                let generation = binding.generation;
                let opened = if let Some(worker) = worker {
                    Ok(binding.browser.resume_with_worker(worker))
                } else {
                    binding.browser.resume(opener, editor_policy)
                };
                return match opened {
                    Ok(task) => completion(owner, tab_id, generation, task),
                    Err(error) => {
                        self.notice(owner, format!("Could not reopen Remote Files ({error})"));
                        Task::none()
                    }
                };
            }
            return Task::none();
        }
        if managed {
            if let Some(binding) = self
                .active_tab_mut(owner)
                .and_then(|tab| tab.remote_files.binding.take())
            {
                self.stop_remote_browser(binding.browser);
            }
            return Task::none();
        }
        let generation = self.allocate();
        let mut follow = Follow::default();
        if frame.scroll_offset == 0 {
            follow.observe(activity, &frame.screen, &user, &host, now);
        }
        let initial = follow.initial_path();
        let opened = if let Some(worker) = self.take_terminal_file_worker(target) {
            Ok(Browser::with_worker(target, worker, initial))
        } else {
            Browser::open(target, opener, initial, editor_policy)
        };
        let dock = &mut self
            .active_tab_mut(owner)
            .expect("visible tab")
            .remote_files;
        let old = dock.binding.take();
        dock.reset_selection();
        dock.pending_drop = None;
        let task = match opened {
            Ok((browser, task)) => {
                dock.binding = Some(Binding {
                    generation,
                    browser,
                    follow,
                    listing: 0,
                    manager_loan: None,
                });
                completion(owner, tab_id, generation, task)
            }
            Err(error) => {
                self.notice(
                    owner,
                    format!("Could not open the Remote Files sidebar ({error})"),
                );
                Task::none()
            }
        };
        if let Some(old) = old {
            self.stop_remote_browser(old.browser);
        }
        task
    }

    pub(super) fn dispose_remote_dock(&mut self, tab: &mut Tab) {
        if let Some(binding) = tab.remote_files.binding.take() {
            self.stop_remote_browser(binding.browser);
        }
    }

    pub(super) fn stop_all_remote_docks(&mut self) {
        let browsers = self
            .windows
            .values_mut()
            .flat_map(Window::terminal_tabs_mut)
            .filter_map(|tab| {
                std::mem::take(&mut tab.remote_files)
                    .binding
                    .map(|binding| binding.browser)
            })
            .collect::<Vec<_>>();
        for browser in browsers {
            self.stop_remote_browser(browser);
        }
    }

    pub(super) fn remote_dock_session_removed(&mut self, id: SessionId) {
        let browsers = self
            .windows
            .values_mut()
            .flat_map(Window::terminal_tabs_mut)
            .filter_map(|tab| {
                tab.remote_files
                    .binding
                    .take_if(|binding| binding.browser.session() == id)
            })
            .map(|binding| binding.browser)
            .collect::<Vec<_>>();
        for browser in browsers {
            self.stop_remote_browser(browser);
        }
    }

    pub(super) fn remote_manager_owns(&self, session: SessionId) -> bool {
        self.snippet_transfer_owns_sftp(session)
            || self.remote_manager_closing.contains_key(&session)
            || self
                .sftp
                .values()
                .any(|state| state.attached_to() == Some(session))
    }

    pub(super) fn release_remote_dock_for_snippet_transfer(
        &mut self,
        session: SessionId,
    ) -> Result<Option<Receiver<()>>, &'static str> {
        if self.remote_manager_owns(session) || self.terminal_file_uses_sftp(session) {
            return Err(
                "Close the attached SFTP Manager or terminal file editor before copying snippets.",
            );
        }
        let closing = self.remote_browser_closing.get(&session).cloned();
        let binding = self
            .windows
            .values_mut()
            .flat_map(Window::terminal_tabs_mut)
            .filter_map(|tab| tab.remote_files.binding.as_mut())
            .find(|binding| binding.browser.session() == session);
        if let Some(binding) = binding {
            if binding.browser.busy() || binding.browser.transfers_active() {
                return Err(
                    "Wait for the Remote Files operation to finish before copying snippets.",
                );
            }
            if let Some(done) = binding.browser.release() {
                binding.manager_loan = Some(ManagerLoan::Manager);
                binding.follow.cancel_listing();
                self.track_session_shutdown(done.clone());
                return Ok(Some(done));
            }
        }
        Ok(closing)
    }

    pub(super) fn lend_remote_dock_to_manager(
        &mut self,
        owner: window::Id,
        session: SessionId,
        path: Option<Vec<u8>>,
    ) -> Option<Task<Desktop>> {
        let editor_open = self.terminal_file_editor_open(session);
        let (tab, _) = self.windows.get(&owner)?.pane_location(session)?;
        let tab = self
            .windows
            .get_mut(&owner)?
            .tabs
            .get_mut(tab)?
            .terminal_mut()?;
        let tab_id = tab.id;
        let binding = tab.remote_files.binding.as_mut()?;
        if binding.browser.session() != session {
            return None;
        }
        if matches!(binding.manager_loan, Some(ManagerLoan::Opening(_))) {
            return Some(Task::none());
        }
        if !binding.browser.has_worker() {
            return None;
        }
        if binding.browser.transfers_active() {
            self.notice(
                owner,
                "Finish the Remote Files transfer before opening SFTP Manager.".into(),
            );
            return Some(Task::none());
        }
        if editor_open {
            if binding.browser.busy() {
                self.notice(
                    owner,
                    "Wait for the current SFTP operation to finish.".into(),
                );
                return Some(Task::none());
            }
            let worker = binding.browser.take_worker()?;
            binding.manager_loan = Some(ManagerLoan::Manager);
            binding.follow.cancel_listing();
            if let Some(worker) = self.retain_terminal_file_worker(session, worker) {
                self.track_session_shutdown(worker.stop());
                return Some(Task::none());
            }
            return self.open_attached_sftp_at(owner, session, path);
        }
        let done = binding.browser.release()?;
        binding.manager_loan = Some(ManagerLoan::Opening(done.clone()));
        binding.follow.cancel_listing();
        let generation = binding.generation;
        self.track_session_shutdown(done.clone());
        Some(Task::future(async move {
            let _ = done.recv().await;
            Desktop::RemoteDock(owner, Message::ManagerReleased(tab_id, generation, path))
        }))
    }

    pub(super) fn remote_manager_releasing(
        &mut self,
        id: window::Id,
        session: SessionId,
        done: Receiver<()>,
    ) -> Task<Desktop> {
        self.remote_manager_closing.insert(session, done.clone());
        Task::future(async move {
            let _ = done.recv().await;
            Desktop::RemoteDock(id, Message::ManagerReturned(session))
        })
    }

    fn remote_manager_released(
        &mut self,
        tab: u64,
        generation: u64,
        path: Option<Vec<u8>>,
    ) -> Task<Desktop> {
        let Some((&owner, binding)) = self.windows.iter_mut().find_map(|(owner, window)| {
            let binding = window
                .terminal_tabs_mut()
                .find(|candidate| candidate.id == tab)?
                .remote_files
                .binding
                .as_mut()?;
            (binding.generation == generation).then_some((owner, binding))
        }) else {
            return Task::none();
        };
        if !matches!(&binding.manager_loan, Some(ManagerLoan::Opening(done)) if done.is_closed()) {
            return Task::none();
        }
        let session = binding.browser.session();
        binding.manager_loan = Some(ManagerLoan::Manager);
        self.open_attached_sftp_at(owner, session, path)
            .unwrap_or_else(|| self.sync_remote_dock(owner))
    }

    fn stop_remote_browser(&mut self, mut browser: Browser) {
        let source = browser.session();
        let transfers_active = browser.transfers_active();
        let worker = browser.take_worker().and_then(|worker| {
            if transfers_active {
                Some(worker)
            } else {
                self.retain_terminal_file_worker(source, worker)
            }
        });
        if let Some(worker) = worker {
            let done = worker.stop();
            self.remote_browser_closing
                .retain(|_, pending| !pending.is_closed());
            if !done.is_closed() {
                self.remote_browser_closing.insert(source, done.clone());
            }
            self.track_session_shutdown(done);
        }
    }

    pub(super) fn wait_for_remote_browser(
        &self,
        owner: window::Id,
        session: SessionId,
        path: Option<Vec<u8>>,
    ) -> Option<Task<Desktop>> {
        let done = self.remote_browser_closing.get(&session)?.clone();
        if done.is_closed() {
            return None;
        }
        Some(Task::future(async move {
            let _ = done.recv().await;
            Desktop::RemoteDock(owner, Message::ManagerAfterBrowser(session, path))
        }))
    }

    pub(super) fn remote_dock_blocks_reconnect(&self, session: SessionId) -> bool {
        self.windows
            .values()
            .flat_map(Window::terminal_tabs)
            .any(|tab| {
                tab.remote_files.binding.as_ref().is_some_and(|binding| {
                    binding.browser.session() == session && binding.browser.transfers_active()
                })
            })
    }

    pub(super) fn update_remote_dock(
        &mut self,
        owner: window::Id,
        message: Message,
    ) -> Task<Desktop> {
        match message {
            Message::Completed(tab, generation, result) => {
                return self.remote_completion(tab, generation, result);
            }
            Message::Dialog(message) => return self.update_remote_dialog(owner, message),
            Message::ManagerReleased(tab, generation, path) => {
                return self.remote_manager_released(tab, generation, path);
            }
            Message::ManagerReturned(session) => {
                if self
                    .remote_manager_closing
                    .get(&session)
                    .is_some_and(Receiver::is_closed)
                {
                    self.remote_manager_closing.remove(&session);
                    return self.sync_remote_docks();
                }
            }
            Message::ManagerAfterBrowser(session, path) => {
                return self
                    .open_attached_sftp_at(owner, session, path)
                    .unwrap_or_else(Task::none);
            }
            Message::Modifiers(modifiers) => {
                if let Some(window) = self.windows.get_mut(&owner) {
                    window.remote_modifiers = modifiers;
                }
                if !modifiers.control() {
                    self.commit_tab_cycle(owner);
                }
            }
            Message::Resize(side, width) => {
                if self.window_settings.remote_sidebar_position == side && width.is_finite() {
                    self.window_settings.remote_sidebar_width = width.round().clamp(
                        f32::from(REMOTE_SIDEBAR_MIN_WIDTH),
                        f32::from(REMOTE_SIDEBAR_MAX_WIDTH),
                    ) as u16;
                }
            }
            Message::FinishResize(side) => {
                if self.window_settings.remote_sidebar_position == side {
                    self.save_remote_sidebar_width(
                        self.window_settings.remote_sidebar_width,
                        owner,
                    );
                }
            }
            Message::Action(tab, action) => return self.remote_dock_action(owner, tab, action),
        }
        Task::none()
    }

    fn remote_completion(
        &mut self,
        tab_id: u64,
        generation: u64,
        result: Completion,
    ) -> Task<Desktop> {
        let Some((&owner, window)) = self
            .windows
            .iter_mut()
            .find(|(_, window)| window.terminal_tabs().any(|tab| tab.id == tab_id))
        else {
            return Task::none();
        };
        let active = window.tab().is_some_and(|tab| tab.id == tab_id);
        let tab = window
            .terminal_tabs_mut()
            .find(|tab| tab.id == tab_id)
            .expect("validated tab");
        let dock = &mut tab.remote_files;
        let Some(binding) = dock
            .binding
            .as_mut()
            .filter(|binding| binding.generation == generation)
        else {
            return Task::none();
        };
        let before = binding.browser.listing_revision();
        if !self.clipboard.policy.file_transfer_allowed {
            binding.browser.cancel_pending_transfers();
        }
        let next = binding.browser.finish(result);
        if binding.browser.listing_revision() != before {
            binding.listing = binding.listing.wrapping_add(1);
            dock.reset_selection();
        }
        let next = completion(owner, tab_id, generation, next);
        if active {
            Task::batch([next, self.sync_remote_dock(owner)])
        } else {
            next
        }
    }

    fn remote_dock_action(
        &mut self,
        owner: window::Id,
        tab_id: u64,
        action: Action,
    ) -> Task<Desktop> {
        if self.window_settings.remote_sidebar_position == RemoteSidebarPosition::Hidden
            || self.owner_dialog(owner).is_some()
        {
            return Task::none();
        }
        let Some(window) = self.windows.get(&owner) else {
            return Task::none();
        };
        let Some(tab) = window.tab().filter(|tab| tab.id == tab_id) else {
            return Task::none();
        };
        let modifiers = window.remote_modifiers;
        if let Action::Hide = action {
            return self.set_remote_sidebar_position(RemoteSidebarPosition::Hidden, Some(owner));
        }
        let Some(binding) = &tab.remote_files.binding else {
            return Task::none();
        };
        let session = binding.browser.session();
        if self.remote_pane(session).is_none() {
            return Task::none();
        }
        let transfer_allowed = self.clipboard.policy.file_transfer_allowed;
        if matches!(
            action,
            Action::Upload(..) | Action::Download(..) | Action::Dropped(..) | Action::Resolve(..)
        ) && !transfer_allowed
        {
            return Task::none();
        }
        match action {
            Action::OpenManager(generation, listing)
                if binding.generation == generation && binding.listing == listing =>
            {
                return binding.browser.path().map(<[u8]>::to_vec).map_or_else(
                    Task::none,
                    |path| {
                        self.open_attached_sftp_at(owner, session, Some(path))
                            .unwrap_or_else(Task::none)
                    },
                );
            }
            Action::Upload(generation, listing) | Action::Download(generation, listing)
                if binding.action_matches(generation, listing) =>
            {
                return self.choose_remote_transfer(
                    owner,
                    tab_id,
                    matches!(action, Action::Upload(..)),
                );
            }
            _ => {}
        }
        let dock = &mut self
            .active_tab_mut(owner)
            .expect("validated tab")
            .remote_files;
        if let Action::Select(generation, listing, name) = action {
            if dock
                .binding
                .as_ref()
                .is_some_and(|binding| binding.action_matches(generation, listing))
            {
                dock.select(name, modifiers);
            }
            return Task::none();
        }
        let binding = dock.binding.as_mut().expect("validated remote binding");
        let generation = binding.generation;
        let task = match action {
            Action::Pin => {
                binding.follow.toggle_pin(Instant::now());
                return self.sync_remote_dock(owner);
            }
            Action::Filter(filter) if filter.len() <= 1024 => {
                dock.filter = filter;
                dock.page = 0;
                Task::none()
            }
            Action::Page(next) => {
                if next {
                    let filter = dock.filter.trim().to_lowercase();
                    let count = binding
                        .browser
                        .entries()
                        .iter()
                        .filter(|entry| {
                            filter.is_empty() || entry.display.to_lowercase().contains(&filter)
                        })
                        .count();
                    if count > (dock.page + 1) * PAGE {
                        dock.page += 1;
                    }
                } else {
                    dock.page = dock.page.saturating_sub(1);
                }
                Task::none()
            }
            Action::Open(source, listing, EntryName::Remote(name))
                if binding.action_matches(source, listing) =>
            {
                if binding.browser.entries().iter().any(|entry| {
                    entry.name == EntryName::Remote(name.clone())
                        && entry.kind == EntryKind::Directory
                }) {
                    binding.follow.cancel_listing();
                    binding
                        .browser
                        .path()
                        .map(|path| follow::join(path, &name))
                        .map_or_else(Task::none, |path| binding.browser.browse(path))
                } else {
                    Task::none()
                }
            }
            Action::Browse(source, listing, path) if binding.action_matches(source, listing) => {
                if binding
                    .browser
                    .path()
                    .is_some_and(|current| is_breadcrumb(current, &path))
                {
                    binding.follow.cancel_listing();
                    binding.browser.browse(path)
                } else {
                    Task::none()
                }
            }
            Action::Refresh if binding.browser.connected() => binding.browser.refresh(),
            Action::Refresh => {
                let old = dock.binding.take().expect("validated remote binding");
                self.stop_remote_browser(old.browser);
                return self.sync_remote_dock(owner);
            }
            Action::Dropped(source, listing, path)
                if binding.action_matches(source, listing)
                    && path.is_absolute()
                    && !binding.browser.busy() =>
            {
                if let Some(target) = binding.target(tab_id) {
                    let now = Instant::now();
                    let pending = dock.pending_drop.get_or_insert_with(|| PendingDrop {
                        target: target.clone(),
                        paths: Vec::new(),
                        ready: now,
                    });
                    if pending.target == target && pending.paths.len() < MAX_DROP_FILES {
                        if !pending.paths.contains(&path) {
                            pending.paths.push(path);
                        }
                        pending.ready = now + Duration::from_millis(80);
                    }
                }
                Task::none()
            }
            Action::Resolve(source, request, choice)
                if source == generation && binding.browser.conflict_revision() == Some(request) =>
            {
                binding.browser.resolve(choice)
            }
            _ => Task::none(),
        };
        completion(owner, tab_id, generation, task)
    }

    pub(super) fn remote_dock_tick(&mut self) -> Task<Desktop> {
        let now = Instant::now();
        let mut tasks = Vec::new();
        let closing = self.remote_browser_closing.len();
        self.remote_browser_closing
            .retain(|_, done| !done.is_closed());
        if closing != self.remote_browser_closing.len() {
            tasks.push(self.sync_remote_docks());
        }
        let mut drops = Vec::new();
        for (&owner, window) in &mut self.windows {
            let Some(tab) = window.tab_mut() else {
                continue;
            };
            let dock = &mut tab.remote_files;
            if let Some(drop) = dock.pending_drop.take_if(|pending| pending.ready <= now) {
                drops.push((owner, drop));
            }
            if !self.remote_dialogs.has_owner(owner)
                && let Some(binding) = &mut dock.binding
                && binding.manager_loan.is_none()
                && let Some(path) = binding.follow.ready_path(now)
                && binding.browser.path() != Some(path.as_slice())
            {
                tasks.push(completion(
                    owner,
                    tab.id,
                    binding.generation,
                    binding.browser.browse(path),
                ));
            }
        }
        for (owner, pending) in drops {
            tasks.push(self.confirm_dropped_remote_upload(owner, pending.target, pending.paths));
        }
        Task::batch(tasks)
    }
}

fn completion(
    owner: window::Id,
    tab: u64,
    generation: u64,
    task: Task<Completion>,
) -> Task<Desktop> {
    task.map(move |reply| Desktop::RemoteDock(owner, Message::Completed(tab, generation, reply)))
}

fn dock_clock() -> BoxStream<'static, Desktop> {
    let (send, receive) = async_channel::bounded(1);
    let _ = std::thread::Builder::new()
        .name("kortty-remote-dock-clock".into())
        .spawn(move || {
            while !send.is_closed() {
                std::thread::sleep(Duration::from_millis(50));
                let _ = send.try_send(());
            }
        });
    receive.map(|()| Desktop::RemoteDockTick).boxed()
}

fn is_breadcrumb(current: &[u8], path: &[u8]) -> bool {
    path == b"/"
        || current == path
        || current
            .strip_prefix(path)
            .is_some_and(|tail| tail.starts_with(b"/"))
}
