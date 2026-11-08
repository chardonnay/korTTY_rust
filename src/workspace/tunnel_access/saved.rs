use std::path::Path;
use std::sync::Arc;

use iced::{Task, window};

use crate::connection_store::{Connection, MAX_TUNNELS, Store, TunnelPlan};
use crate::profile;
use crate::session::SessionId;
use crate::workspace::Window;

use super::{Forwarding, Message, Workspace, WorkspaceMessage};

mod handoff;
pub(in crate::workspace) use handoff::ClosingOwner;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedReview {
    session: SessionId,
    connection: u64,
    path: Arc<Path>,
    revision: u64,
    pub(super) plan: TunnelPlan,
}

impl SavedReview {
    fn same_plan(&self, other: &Self) -> bool {
        self.connection == other.connection && self.path == other.path && self.plan == other.plan
    }

    fn persist(&self) -> Result<(), String> {
        Store::open(self.path.to_path_buf())
            .and_then(|mut store| {
                if store.approved(self.connection, &self.plan) {
                    return Ok(());
                }
                store.approve(self.connection, self.plan.clone())
            })
            .map_err(|error| error.to_string())
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Phase {
    #[default]
    Evaluated,
    Review,
    Approving,
    Ready,
}

pub(super) struct SavedTunnels {
    connection: u64,
    path: Arc<Path>,
    revision: u64,
    review: Option<SavedReview>,
    phase: Phase,
    attached: bool,
    stopped: [bool; MAX_TUNNELS],
}

impl SavedTunnels {
    fn new(connection: u64, path: &Path) -> Self {
        Self {
            connection,
            path: Arc::from(path),
            revision: 0,
            review: None,
            phase: Phase::Evaluated,
            attached: false,
            stopped: [false; MAX_TUNNELS],
        }
    }

    fn change(&mut self, session: SessionId, store: &Store, plan: Option<TunnelPlan>) -> bool {
        if self.review.as_ref().map(|review| &review.plan) == plan.as_ref() {
            return false;
        }
        self.revision += 1;
        self.attached = false;
        self.stopped = [false; MAX_TUNNELS];
        self.review = plan.map(|plan| SavedReview {
            session,
            connection: self.connection,
            path: self.path.clone(),
            revision: self.revision,
            plan,
        });
        self.phase = match &self.review {
            Some(review) if store.approved(self.connection, &review.plan) => Phase::Ready,
            Some(_) => Phase::Review,
            None => Phase::Evaluated,
        };
        true
    }

    pub(super) fn stop_rule(&mut self, index: usize) {
        self.stopped[index] = true;
    }
}

impl Forwarding {
    fn retire_saved(&mut self) {
        for entry in self.active.values().filter(|entry| entry.saved.is_some()) {
            entry.listener.stop();
            self.retiring_saved = true;
        }
    }

    fn retire_complete(&mut self) -> bool {
        if self.retiring_saved {
            self.active
                .retain(|_, entry| entry.saved.is_none() || !entry.listener.is_finished());
            self.retiring_saved = self.active.values().any(|entry| entry.saved.is_some());
        }
        !self.retiring_saved
    }

    fn advance_saved(&mut self, session: SessionId, allowed: bool) -> Task<WorkspaceMessage> {
        if !self.retire_complete() {
            return Task::none();
        }
        let Some(saved) = self
            .saved
            .as_mut()
            .filter(|saved| saved.phase == Phase::Ready)
        else {
            return Task::none();
        };
        saved.phase = Phase::Evaluated;
        let Some(review) = saved
            .review
            .as_ref()
            .filter(|_| allowed && !self.direct.is_closed())
        else {
            return Task::none();
        };
        if !Store::open(review.path.to_path_buf())
            .is_ok_and(|store| store.approved(review.connection, &review.plan))
        {
            self.notice = Some("The saved SSH tunnels changed before they could start.".into());
            return Task::none();
        }
        let plan = review.plan.clone();
        saved.attached = true;
        let stopped = saved.stopped;
        self.start_saved(session, plan, stopped)
    }
}

impl Workspace {
    pub(in crate::workspace) fn configure_saved_tunnels(
        &mut self,
        window: window::Id,
        session: SessionId,
        connection: u64,
    ) -> Task<WorkspaceMessage> {
        if self
            .windows
            .get(&window)
            .and_then(|state| {
                state
                    .pane_location(session)
                    .and_then(|(tab, _)| state.tabs[tab].terminal())
            })
            .is_none_or(|tab| tab.tunnel_owner != Some(session))
        {
            return Task::none();
        }
        let Ok(path) = profile::connections_path() else {
            return Task::none();
        };
        let Some(forwarding) = self
            .sessions
            .get_mut(&session)
            .and_then(|pane| pane.forwarding.as_mut())
        else {
            return Task::none();
        };
        forwarding
            .saved
            .get_or_insert_with(|| SavedTunnels::new(connection, &path));
        Store::open(path).map_or_else(
            |_| Task::none(),
            |store| self.apply_saved_to(session, &store),
        )
    }

    pub(in crate::workspace) fn apply_saved_tunnel_profiles(
        &mut self,
        store: &Store,
    ) -> Task<WorkspaceMessage> {
        let mut sessions: Vec<_> = self
            .sessions
            .iter()
            .filter_map(|(&session, pane)| {
                pane.forwarding.as_ref()?.saved.as_ref().map(|_| session)
            })
            .collect();
        sessions.sort_unstable_by_key(|session| session.0);
        Task::batch(
            sessions
                .into_iter()
                .map(|session| self.apply_saved_to(session, store)),
        )
    }

    fn apply_saved_to(&mut self, session: SessionId, store: &Store) -> Task<WorkspaceMessage> {
        let allowed = self.clipboard.policy.port_forwarding_allowed;
        let Some(pane) = self.sessions.get_mut(&session) else {
            return Task::none();
        };
        let Some(forwarding) = &mut pane.forwarding else {
            return Task::none();
        };
        let Some(saved) = forwarding
            .saved
            .as_mut()
            .filter(|saved| saved.path.as_ref() == store.path())
        else {
            return Task::none();
        };
        let plan = store
            .get(saved.connection)
            .filter(|profile| {
                allowed
                    && pane
                        .ssh
                        .as_ref()
                        .is_some_and(|target| target.matches(profile))
            })
            .map(Connection::plan)
            .filter(|plan| !plan.rules.is_empty());
        if !saved.change(session, store, plan) {
            return Task::none();
        }
        if saved.phase == Phase::Review
            && !forwarding.direct.is_closed()
            && let Some(window) = self
                .windows
                .values_mut()
                .find(|state| state.pane_location(session).is_some())
        {
            window.cancel_paste();
        }
        forwarding.retire_saved();
        self.advance_saved_tunnels(session)
    }

    pub(super) fn advance_saved_tunnels(&mut self, session: SessionId) -> Task<WorkspaceMessage> {
        if self
            .closing_tunnel_owners
            .values()
            .any(|owner| owner.destination == session)
        {
            return Task::none();
        }
        let allowed = self.clipboard.policy.port_forwarding_allowed;
        self.sessions
            .get_mut(&session)
            .and_then(|pane| pane.forwarding.as_mut())
            .map_or_else(Task::none, |forwarding| {
                forwarding.advance_saved(session, allowed)
            })
    }

    pub(in crate::workspace) fn pending_tunnel_review(
        &self,
        window: &Window,
    ) -> Option<&SavedReview> {
        if !self.clipboard.policy.port_forwarding_allowed {
            return None;
        }
        window
            .terminal_tabs()
            .flat_map(|tab| tab.grid.iter())
            .filter_map(|(_, session)| {
                let forwarding = self.sessions.get(session)?.forwarding.as_ref()?;
                let saved = forwarding.saved.as_ref()?;
                if forwarding.direct.is_closed()
                    || !matches!(saved.phase, Phase::Review | Phase::Approving)
                {
                    return None;
                }
                saved.review.as_ref()
            })
            .min_by_key(|review| review.session.0)
    }

    pub(super) fn tunnel_approval_pending(&self, review: &SavedReview) -> bool {
        self.sessions
            .get(&review.session)
            .and_then(|pane| pane.forwarding.as_ref())
            .and_then(|forwarding| forwarding.saved.as_ref())
            .is_some_and(|saved| {
                saved.phase == Phase::Approving && saved.review.as_ref() == Some(review)
            })
    }

    pub(super) fn review_tunnels(
        &mut self,
        window: window::Id,
        review: SavedReview,
        allowed: bool,
    ) -> Task<WorkspaceMessage> {
        if self
            .windows
            .get(&window)
            .and_then(|state| self.pending_tunnel_review(state))
            != Some(&review)
        {
            return Task::none();
        }
        let Some(saved) = self
            .sessions
            .get_mut(&review.session)
            .and_then(|pane| pane.forwarding.as_mut())
            .and_then(|forwarding| forwarding.saved.as_mut())
            .filter(|saved| saved.phase == Phase::Review)
        else {
            return Task::none();
        };
        saved.phase = if allowed {
            Phase::Approving
        } else {
            Phase::Evaluated
        };
        if !allowed {
            return Task::none();
        }
        Task::future(async move {
            let result = review.persist();
            WorkspaceMessage::Tunnel(Message::Approved(review, result))
        })
    }

    pub(super) fn tunnel_approval_result(
        &mut self,
        review: SavedReview,
        result: Result<(), String>,
    ) -> Task<WorkspaceMessage> {
        // The completion may arrive after the connection manager saved a new revision.
        let latest = Store::open(review.path.to_path_buf());
        let approved = latest
            .as_ref()
            .is_ok_and(|store| store.approved(review.connection, &review.plan));
        if let Ok(store) = latest {
            self.update_connection_store(store);
        }
        if !self.tunnel_approval_pending(&review) {
            return Task::none();
        }
        // A different window can finish writing the exact same approval first.
        if !approved {
            let error = result
                .err()
                .unwrap_or_else(|| "saved tunnel plan changed".into());
            if let Some(forwarding) = self
                .sessions
                .get_mut(&review.session)
                .and_then(|pane| pane.forwarding.as_mut())
                && let Some(saved) = &mut forwarding.saved
            {
                saved.phase = Phase::Evaluated;
            }
            if let Some((&id, _)) = self
                .windows
                .iter()
                .find(|(_, state)| state.pane_location(review.session).is_some())
            {
                self.notice(id, format!("Could not approve saved tunnels ({error})"));
            }
            return Task::none();
        }
        let mut ready = Vec::new();
        for (&session, pane) in &mut self.sessions {
            if let Some(saved) = pane
                .forwarding
                .as_mut()
                .and_then(|forwarding| forwarding.saved.as_mut())
                && matches!(saved.phase, Phase::Review | Phase::Approving)
                && saved
                    .review
                    .as_ref()
                    .is_some_and(|candidate| candidate.same_plan(&review))
            {
                saved.phase = Phase::Ready;
                ready.push(session);
            }
        }
        ready.sort_unstable_by_key(|session| (session != &review.session, session.0));
        Task::batch(
            ready
                .into_iter()
                .map(|session| self.advance_saved_tunnels(session)),
        )
    }
}

#[cfg(test)]
mod tests;
