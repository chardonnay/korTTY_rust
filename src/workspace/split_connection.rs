use iced::widget::pane_grid;
use iced::{Task, window};

use crate::session::{SessionId, Status};

use super::{Message, Workspace};

#[derive(Clone, Copy)]
pub(super) struct SplitTarget {
    tab: u64,
    pub(super) source: SessionId,
    axis: pane_grid::Axis,
    pub(super) same_server: bool,
}

#[derive(Clone, Copy)]
pub(super) struct PendingSplitConnection {
    owner: window::Id,
    target: SplitTarget,
}

impl Workspace {
    pub(super) fn split_connection_allowed(&self, owner: window::Id, source: SessionId) -> bool {
        self.clipboard.policy.problem.is_none()
            && self.quick_connect.is_none()
            && !self.palette.is_open(owner)
            && self.palette_can_open(owner)
            && self.sessions.contains_key(&source)
            && self.window_contains_visible_pane(owner, source)
    }

    pub(super) fn show_split_connection(
        &mut self,
        owner: window::Id,
        source: SessionId,
        axis: pane_grid::Axis,
    ) -> Task<Message> {
        if !self.split_connection_allowed(owner, source) {
            return Task::none();
        }
        let Some(tab) = self.windows.get(&owner).and_then(super::Window::tab) else {
            return Task::none();
        };
        let target = SplitTarget {
            tab: tab.id,
            source,
            axis,
            same_server: false,
        };
        self.open_quick_connect(owner, Some(target))
    }

    pub(super) fn same_server_split_allowed(&self, owner: window::Id, source: SessionId) -> bool {
        if self.clipboard.policy.problem.is_some() || !self.palette_can_open(owner) {
            return false;
        }
        let Some(window) = self.windows.get(&owner) else {
            return false;
        };
        let Some(tab) = window.tab() else {
            return false;
        };
        let Some(pane) = self.sessions.get(&source) else {
            return false;
        };
        if (tab.incognito && !self.clipboard.policy.incognito_allowed)
            || self.visible_pane(owner, source).is_none()
            || pane.frame.ibm.is_some()
        {
            return false;
        }
        if pane.frame.serial.is_some() {
            return self.clipboard.policy.serial_allowed
                && !self.isolation_required()
                && !matches!(pane.frame.status, Status::Starting | Status::Running);
        }
        pane.ssh.is_some() || pane.local_security.is_some()
    }

    pub(super) fn split_same_server(
        &mut self,
        owner: window::Id,
        source: SessionId,
        axis: pane_grid::Axis,
    ) -> Task<Message> {
        if !self.same_server_split_allowed(owner, source) {
            return Task::none();
        }
        let Some(tab) = self.windows.get(&owner).and_then(super::Window::tab) else {
            return Task::none();
        };
        let target = SplitTarget {
            tab: tab.id,
            source,
            axis,
            same_server: true,
        };
        let incognito = tab.incognito;
        let Some(pane) = self.sessions.get(&source) else {
            return Task::none();
        };
        let profile = pane.frame.screen.profile;
        let font = pane.font_size_override();
        if let Some(ssh) = pane.ssh.clone() {
            return self.start_same_ssh_split(owner, target, &ssh, profile, incognito, font);
        }
        if let Some(serial) = pane.frame.serial.as_deref().cloned() {
            let form = crate::serial_dialog::SerialDialog::new(self.allocate(), serial, profile);
            if let Some(window) = self.windows.get_mut(&owner) {
                window.pending_serial = Some(super::PendingSerial {
                    form,
                    replace: None,
                    reconnect: None,
                    split: Some(target),
                    font,
                });
            }
            return self.connect_serial(owner);
        } else {
            let settings = pane.local_security.clone();
            match self.store_local_session(profile, font, settings) {
                Ok(session) => self.start_split_worker(owner, target, session),
                Err(error) => self.notice(
                    owner,
                    format!("Could not start a terminal worker ({error})"),
                ),
            }
        }
        Task::none()
    }

    pub(super) fn split_target_pane(
        &self,
        owner: window::Id,
        target: SplitTarget,
    ) -> Option<pane_grid::Pane> {
        let window = self.windows.get(&owner)?;
        let tab = window.tab()?;
        if tab.id != target.tab || !self.sessions.contains_key(&target.source) {
            return None;
        }
        self.visible_pane(owner, target.source)
    }

    pub(super) fn split_connection_privacy(
        &self,
        owner: window::Id,
        target: SplitTarget,
        requested: bool,
    ) -> Result<bool, &'static str> {
        self.split_target_pane(owner, target)
            .ok_or("The original pane is no longer available for splitting.")?;
        let incognito = self.windows[&owner].tab().is_some_and(|tab| tab.incognito);
        if requested && !incognito {
            return Err("Open an incognito tab before splitting to this connection.");
        }
        if incognito && !self.clipboard.policy.incognito_allowed {
            return Err("Managed policy blocks incognito sessions.");
        }
        Ok(incognito)
    }

    pub(super) fn start_split_worker(
        &mut self,
        owner: window::Id,
        target: SplitTarget,
        session: SessionId,
    ) {
        if self.split_target_pane(owner, target).is_none()
            || self.windows[&owner].pending_split_connection.is_some()
        {
            self.stop_session(session);
            return;
        }
        if let Some(pane) = self.sessions.get_mut(&session)
            && let Some(window) = self.windows.get_mut(&owner)
        {
            pane.pending_split_connection = Some(PendingSplitConnection { owner, target });
            window.pending_split_connection = Some(session);
            window.notice = None;
        }
    }

    fn clear_pending_split(&mut self, owner: window::Id, session: SessionId) -> bool {
        let removed = self.windows.get_mut(&owner).and_then(|window| {
            window
                .pending_split_connection
                .take_if(|pending| *pending == session)
        });
        if removed.is_some()
            && let Some(pane) = self.sessions.get_mut(&session)
        {
            pane.pending_split_connection = None;
        }
        removed.is_some()
    }

    pub(super) fn cancel_split_connection(&mut self, owner: window::Id, session: SessionId) {
        if self.clear_pending_split(owner, session) {
            self.stop_session(session);
        }
    }

    pub(super) fn cancel_split_for_tab(&mut self, owner: window::Id, tab: u64) {
        if let Some(session) = self
            .windows
            .get(&owner)
            .and_then(|window| window.pending_split_connection)
            && self
                .sessions
                .get(&session)
                .and_then(|pane| pane.pending_split_connection)
                .is_some_and(|pending| pending.target.tab == tab)
        {
            self.cancel_split_connection(owner, session);
        }
    }

    pub(super) fn cancel_split_source(&mut self, source: SessionId) {
        let pending = self.windows.iter().find_map(|(&owner, window)| {
            let session = window.pending_split_connection?;
            let pending = self.sessions.get(&session)?.pending_split_connection?;
            (pending.target.source == source).then_some((owner, session))
        });
        if let Some((owner, session)) = pending {
            self.cancel_split_connection(owner, session);
        }
    }

    pub(super) fn finish_split_connection(&mut self, session: SessionId) {
        let Some(pane) = self.sessions.get(&session) else {
            return;
        };
        let Some(PendingSplitConnection { owner, target }) = pane.pending_split_connection else {
            return;
        };
        let valid = self.split_target_pane(owner, target).is_some();
        if valid && pane.frame.status == Status::Starting {
            return;
        }
        let failure = match &pane.frame.status {
            Status::Running if valid => None,
            Status::Error(error) if valid => Some(format!("Could not open the split ({error})")),
            _ if valid => Some("The connection closed before the split was ready.".into()),
            _ => Some("The original pane is no longer available for splitting.".into()),
        };
        if !self.clear_pending_split(owner, session) {
            self.stop_session(session);
        } else if let Some(failure) = failure {
            self.stop_session(session);
            self.notice(owner, failure);
        } else {
            self.attach_split_connection(owner, target, session);
        }
    }

    pub(super) fn attach_split_connection(
        &mut self,
        owner: window::Id,
        target: SplitTarget,
        session: SessionId,
    ) {
        let attached = self.split_target_pane(owner, target).and_then(|source| {
            let tab = self.active_tab_mut(owner)?;
            let (pane, _) = tab.grid.split(target.axis, source, session)?;
            tab.grid.restore();
            tab.focused = pane;
            Some(())
        });
        if attached.is_some() {
            self.inherit_terminal_effect(target.source, session);
            self.session_restore.changed(false);
            self.refresh_connection_counts();
        } else {
            self.stop_session(session);
            self.notice(
                owner,
                "The original pane is no longer available for splitting.".into(),
            );
        }
    }
}

#[cfg(test)]
impl Workspace {
    pub(super) fn wait_for_test_split(&mut self, owner: window::Id) {
        use std::time::{Duration, Instant};

        let deadline = Instant::now() + Duration::from_secs(6);
        while let Some(session) = self.windows[&owner].pending_split_connection {
            assert!(Instant::now() < deadline, "split worker did not finish");
            let _ = self.update(Message::SessionUpdate(session));
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
