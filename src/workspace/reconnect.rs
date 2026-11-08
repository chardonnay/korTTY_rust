use iced::widget::pane_grid;
use iced::{Task, window};

use crate::session::{SessionId, Status};

use super::{
    Message, PaneSession, Tab, Workspace,
    ssh_access::{KeySnapshots, SavedSshProfile},
};

#[cfg(test)]
mod tests;
pub(super) mod transport;

const RECONNECTING: &str = "Reconnecting…";
pub(super) const SFTP_BUSY: &str =
    "Finish the attached SFTP operation or close its remote editor before reconnecting.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ReconnectTarget {
    pub(super) tab: u64,
    pub(super) source: SessionId,
}

impl ReconnectTarget {
    fn pane(self, tab: &Tab) -> Option<pane_grid::Pane> {
        tab.grid
            .iter()
            .find_map(|(pane, session)| (*session == self.source).then_some(*pane))
    }
}

pub(super) struct PendingReconnect {
    owner: window::Id,
    target: ReconnectTarget,
    saved: Option<Box<SavedSshProfile>>,
    key_snapshots: KeySnapshots,
    transferring_history: bool,
}

impl Tab {
    pub(super) fn primary_pane_index(&self) -> Option<usize> {
        self.pane_index(self.primary)
    }

    pub(super) fn pane_index(&self, target: pane_grid::Pane) -> Option<usize> {
        fn find(node: &pane_grid::Node, target: pane_grid::Pane, index: &mut usize) -> bool {
            match node {
                pane_grid::Node::Pane(pane) if *pane == target => true,
                pane_grid::Node::Pane(_) => {
                    *index += 1;
                    false
                }
                pane_grid::Node::Split { a, b, .. } => {
                    find(a, target, index) || find(b, target, index)
                }
            }
        }

        let mut index = 0;
        find(self.grid.layout(), target, &mut index).then_some(index)
    }
}

impl Workspace {
    fn tab_reconnect_target(&self, owner: window::Id, tab: u64) -> Option<ReconnectTarget> {
        let tab = self
            .windows
            .get(&owner)?
            .terminal_tabs()
            .find(|item| item.id == tab)?;
        Some(ReconnectTarget {
            tab: tab.id,
            source: *tab.grid.get(tab.primary)?,
        })
    }

    pub(super) fn reconnect_target(
        &self,
        owner: window::Id,
        target: ReconnectTarget,
    ) -> Option<(&Tab, &PaneSession)> {
        let tab = self
            .windows
            .get(&owner)?
            .terminal_tabs()
            .find(|tab| tab.id == target.tab)?;
        if self.exiting
            || self.scheduler_draining
            || self.clipboard.policy.problem.is_some()
            || (tab.incognito && !self.clipboard.policy.incognito_allowed)
            || target.pane(tab).is_none()
        {
            return None;
        }
        let pane = self.sessions.get(&target.source)?;
        if pane.frame.serial.is_some()
            && (!self.clipboard.policy.serial_allowed || self.isolation_required())
            || pane.frame.ibm.is_some() && self.isolation_required()
        {
            return None;
        }
        Some((tab, pane))
    }

    pub(super) fn can_reconnect_tab(&self, owner: window::Id, tab: u64) -> bool {
        let Some(target) = self.tab_reconnect_target(owner, tab) else {
            return false;
        };
        self.palette_can_open(owner)
            && self.quick_connect.is_none()
            && !self
                .pending_reconnects
                .values()
                .any(|pending| pending.target == target)
            && self
                .reconnect_target(owner, target)
                .is_some_and(|(_, pane)| {
                    pane.session.frame().status != Status::Starting
                        && (pane.ssh.is_some()
                            || pane.local_security.is_some()
                            || pane.frame.serial.is_some()
                            || pane.frame.ibm.is_some())
                })
    }

    pub(super) fn reconnect_worker_pending(
        &self,
        owner: window::Id,
        target: ReconnectTarget,
    ) -> bool {
        self.pending_reconnects
            .values()
            .any(|pending| pending.owner == owner && pending.target == target)
    }

    pub(super) fn cancel_reconnect_worker_for_target(
        &mut self,
        owner: window::Id,
        target: ReconnectTarget,
    ) {
        self.cancel_reconnect_worker(|pending| pending.owner == owner && pending.target == target);
    }

    pub(super) fn reconnect_tab(&mut self, owner: window::Id, tab: u64) -> Task<Message> {
        if !self.can_reconnect_tab(owner, tab) {
            return Task::none();
        }
        let Some(target) = self.tab_reconnect_target(owner, tab) else {
            return Task::none();
        };
        let Some((_, pane)) = self.reconnect_target(owner, target) else {
            return Task::none();
        };
        if pane.ssh.is_some() {
            return self.start_ssh_reconnect(owner, target);
        }
        if pane.frame.serial.is_some() {
            return self.show_serial_reconnect(owner, target);
        }
        if pane.frame.ibm.is_some() {
            return self.show_ibm_reconnect(owner, target);
        }
        let profile = pane.frame.screen.profile;
        let font = pane.font_size_override();
        let security = pane.local_security.clone();
        let (grid, strict) = (pane.grid, pane.strict_escapes);
        match self.store_local_session_in_grid(profile, font, security, grid, strict) {
            Ok(candidate) => {
                self.start_reconnect_worker(owner, target, candidate, None, KeySnapshots::default())
            }
            Err(error) => {
                self.notice(
                    owner,
                    format!("Could not restart the local shell ({error})"),
                );
                Task::none()
            }
        }
    }

    pub(super) fn start_reconnect_worker(
        &mut self,
        owner: window::Id,
        target: ReconnectTarget,
        candidate: SessionId,
        saved: Option<Box<SavedSshProfile>>,
        key_snapshots: KeySnapshots,
    ) -> Task<Message> {
        let Some((_, old)) = self.reconnect_target(owner, target) else {
            self.stop_session(candidate);
            return Task::none();
        };
        let grid = old.grid;
        let strict = old.strict_escapes;
        let Some(next) = self.sessions.get_mut(&candidate) else {
            return Task::none();
        };
        if next.frame.ibm.is_none() {
            next.grid = grid;
            next.session.resize(grid);
            if strict && !next.strict_escapes {
                next.strict_escapes = true;
                next.session.set_strict_escapes(true);
            }
        }
        self.pending_reconnects.insert(
            candidate,
            PendingReconnect {
                owner,
                target,
                saved,
                key_snapshots,
                transferring_history: false,
            },
        );
        self.notice(owner, RECONNECTING.into());
        self.finish_reconnect_worker(candidate)
    }

    pub(super) fn finish_reconnect_worker(&mut self, candidate: SessionId) -> Task<Message> {
        let Some(next) = self.sessions.get(&candidate) else {
            return Task::none();
        };
        if next.frame.status == Status::Starting {
            return Task::none();
        }
        let Some(pending) = self.pending_reconnects.get(&candidate) else {
            return Task::none();
        };
        if next.frame.status == Status::Running {
            if pending.transferring_history {
                return Task::none();
            }
            if let Some(transfer) = self.prepare_reconnect_history(candidate) {
                return Task::perform(transfer, move |ready| {
                    Message::ReconnectHistoryReady(candidate, ready)
                });
            }
        }
        self.complete_reconnect(candidate, true)
    }

    fn prepare_reconnect_history(
        &mut self,
        candidate: SessionId,
    ) -> Option<impl Future<Output = bool> + Send + 'static> {
        let pending = self.pending_reconnects.get_mut(&candidate)?;
        let old = self.sessions.get(&pending.target.source)?;
        let next = self.sessions.get(&candidate)?;
        let transfer = old.session.reconnect_history_to(&next.session)?;
        pending.transferring_history = true;
        Some(transfer)
    }

    pub(super) fn finish_reconnect_history(
        &mut self,
        candidate: SessionId,
        ready: bool,
    ) -> Task<Message> {
        if self
            .pending_reconnects
            .get(&candidate)
            .is_none_or(|pending| !pending.transferring_history)
        {
            return Task::none();
        }
        self.complete_reconnect(candidate, ready)
    }

    fn complete_reconnect(&mut self, candidate: SessionId, history_ready: bool) -> Task<Message> {
        let Some(PendingReconnect {
            owner,
            target,
            saved,
            key_snapshots,
            ..
        }) = self.pending_reconnects.remove(&candidate)
        else {
            return Task::none();
        };
        let Some(next) = self.sessions.get_mut(&candidate) else {
            return Task::none();
        };
        next.frame = next.session.frame();
        let failure = match &next.frame.status {
            Status::Running if !key_snapshots.is_current() => {
                Some("A registered SSH key changed. Reconnect again to use the current key.".into())
            }
            Status::Running if saved.as_ref().is_some_and(|saved| !saved.is_current()) => Some(
                "The saved SSH connection changed. Reconnect again to use the current profile."
                    .into(),
            ),
            Status::Running if !history_ready => {
                Some("Could not carry the previous scrollback into the new session.".into())
            }
            Status::Running => None,
            Status::Error(error) => Some(format!("Could not reconnect ({error})")),
            _ => Some("The new connection closed before it was ready.".into()),
        };
        if let Some(failure) = failure {
            self.project_candidate_failed(owner, target);
            self.stop_session(candidate);
            self.notice(owner, failure);
            Task::none()
        } else {
            self.commit_reconnect(owner, target, candidate)
        }
    }

    fn commit_reconnect(
        &mut self,
        owner: window::Id,
        target: ReconnectTarget,
        candidate: SessionId,
    ) -> Task<Message> {
        let Some((tab, old)) = self.reconnect_target(owner, target) else {
            self.stop_session(candidate);
            self.notice(
                owner,
                "The original terminal is no longer available for reconnecting.".into(),
            );
            return Task::none();
        };
        if old.ssh.is_some() && self.reconnect_sftp_busy(target.source) {
            self.stop_session(candidate);
            self.notice(owner, SFTP_BUSY.into());
            return Task::none();
        }
        let minimum = self.clipboard.policy.minimum_isolation;
        let pane = target.pane(tab).expect("validated reconnect pane");
        let primary = tab.primary == pane;
        let mirrored = self.tab_broadcast_active(tab) || self.multi_exec_member(target.source);
        let owns_tunnels = tab.tunnel_owner == Some(target.source);
        let (grid, font, baseline) = (old.grid, old.font, old.font_baseline);
        let inherits_font_size = old.inherits_font_size;
        let (strict, typeahead) = (old.strict_escapes, old.typeahead_enabled);
        let effect = old.effect;
        let Some(next) = self.sessions.get_mut(&candidate) else {
            return Task::none();
        };
        if next.ssh.as_ref().is_some_and(|ssh| ssh.isolation < minimum)
            || next
                .local_security
                .as_ref()
                .is_some_and(|local| local.isolation < minimum)
        {
            self.stop_session(candidate);
            self.notice(
                owner,
                "The new session no longer meets the required isolation.".into(),
            );
            return Task::none();
        }
        let saved = next.ssh.as_ref().and_then(super::SshTarget::saved_id);
        let ibm = next.ibm_origin.clone();
        let can_mirror = next.frame.ibm.is_none();
        next.font = font;
        next.font_baseline = baseline;
        next.inherits_font_size = inherits_font_size;
        next.effect = effect;
        if next.frame.ibm.is_none() {
            next.grid = grid;
            next.session.resize(grid);
            next.strict_escapes |= strict;
            next.session.set_strict_escapes(next.strict_escapes);
        }
        next.typeahead_enabled = typeahead && next.supports_typeahead();
        next.session
            .set_typeahead_enabled(next.typeahead_enabled && !mirrored);
        if effect.is_some() {
            self.sync_pane_appearance(candidate);
        }

        let window = self
            .windows
            .get_mut(&owner)
            .expect("validated reconnect owner");
        let tab = window
            .terminal_tabs_mut()
            .find(|tab| tab.id == target.tab)
            .expect("validated reconnect tab");
        *tab.grid.get_mut(pane).expect("validated reconnect pane") = candidate;
        if primary
            && let Some(next) = ibm
            && let Some(super::recently_closed::Origin::Ibm(origin, _)) = &mut tab.reopen_origin
            && origin.address.is_none()
        {
            *origin = next;
        }
        if owns_tunnels {
            tab.tunnel_owner = Some(candidate);
        }
        if window.tab().is_some_and(|tab| tab.id == target.tab) {
            window.cancel_paste();
        }
        if window.notice.as_deref() == Some(RECONNECTING) {
            window.notice = None;
        }
        if can_mirror {
            self.multi_exec.replace(target.source, candidate);
        }
        self.session_restore.changed(false);
        self.refresh_connection_counts();

        if owns_tunnels && let Some(saved) = saved {
            self.remap_tunnel_predecessors(target.source, candidate);
            let closing = self.drain_replaced_ssh(target.source, candidate);
            let tunnels = self.configure_saved_tunnels(owner, candidate, saved);
            Task::batch([closing, tunnels])
        } else {
            self.stop_session(target.source);
            Task::none()
        }
    }

    pub(super) fn cancel_reconnect_for_tab(&mut self, owner: window::Id, tab: u64) {
        self.cancel_reconnect_worker(|pending| pending.owner == owner && pending.target.tab == tab);
        self.cancel_ssh_reconnect_for_tab(owner, tab);
    }

    pub(super) fn cancel_reconnect_source(&mut self, source: SessionId) {
        self.cancel_reconnect_worker(|pending| pending.target.source == source);
    }

    fn cancel_reconnect_worker(&mut self, matches: impl Fn(&PendingReconnect) -> bool) {
        let candidates: Vec<_> = self
            .pending_reconnects
            .iter()
            .filter_map(|(&id, pending)| matches(pending).then_some(id))
            .collect();
        for id in candidates {
            let Some(pending) = self.pending_reconnects.remove(&id) else {
                continue;
            };
            if let Some(window) = self.windows.get_mut(&pending.owner)
                && window.notice.as_deref() == Some(RECONNECTING)
            {
                window.notice = None;
            }
            self.stop_session(id);
        }
    }
}

#[cfg(all(test, unix))]
impl Workspace {
    pub(super) fn wait_for_test_reconnect(&mut self, candidate: SessionId) {
        use std::time::{Duration, Instant};

        let deadline = Instant::now() + Duration::from_secs(5);
        while self.sessions[&candidate].session.frame().status == Status::Starting {
            assert!(Instant::now() < deadline, "reconnect worker did not start");
            std::thread::sleep(Duration::from_millis(5));
        }
        if let Some(transfer) = self.prepare_reconnect_history(candidate) {
            let ready = iced::futures::executor::block_on(transfer);
            let _ = self.update(Message::ReconnectHistoryReady(candidate, ready));
        } else {
            let _ = self.update(Message::SessionUpdate(candidate));
        }
    }
}
