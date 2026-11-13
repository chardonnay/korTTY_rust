use async_channel::Receiver;
use iced::{Task, window};

use crate::session::{Session, SessionId, Status};
use crate::ssh_tunnel::Listener;

use super::{Message, Phase, SavedTunnels, Workspace, WorkspaceMessage};

pub(in crate::workspace) struct ClosingOwner {
    transport: ClosingTransport,
    pub(super) destination: SessionId,
}

enum ClosingTransport {
    Draining {
        session: Box<Session>,
        listeners: Vec<Listener>,
    },
    Stopping(Receiver<()>),
}

impl ClosingOwner {
    pub(in crate::workspace) fn stop(self) -> Receiver<()> {
        match self.transport {
            ClosingTransport::Draining { session, .. } => session.stop(),
            ClosingTransport::Stopping(done) => done,
        }
    }

    fn wait_for_shutdown(&mut self, source: SessionId) -> Option<Task<WorkspaceMessage>> {
        match &self.transport {
            ClosingTransport::Draining { listeners, .. }
                if listeners.iter().any(|listener| !listener.is_finished()) =>
            {
                Some(Task::none())
            }
            ClosingTransport::Draining { session, .. } => {
                let done = session.stop();
                self.transport = ClosingTransport::Stopping(done.clone());
                Some(Task::future(async move {
                    let _ = done.recv().await;
                    WorkspaceMessage::Tunnel(Message::ListenerChanged(source))
                }))
            }
            ClosingTransport::Stopping(done) if !done.is_closed() => Some(Task::none()),
            ClosingTransport::Stopping(_) => None,
        }
    }
}

impl SavedTunnels {
    fn can_rehome(&self) -> bool {
        self.review.is_some() && (self.attached || self.phase == Phase::Ready)
    }

    fn reassign(&mut self, destination: SessionId) {
        if let Some(review) = &mut self.review {
            review.session = destination;
        }
        self.phase = Phase::Ready;
        self.attached = false;
    }
}

impl Workspace {
    fn tunnel_successor(&self, window: window::Id, source: SessionId) -> Option<SessionId> {
        if !self.clipboard.policy.port_forwarding_allowed {
            return None;
        }
        let tab = self.windows.get(&window)?.tab()?;
        let old = self.sessions.get(&source)?;
        let old_route = old.ssh.as_ref()?;
        let saved = old.forwarding.as_ref()?.saved.as_ref()?;
        if !saved.can_rehome() {
            return None;
        }
        tab.grid
            .iter()
            .filter_map(|(_, &session)| {
                let pane = self.sessions.get(&session)?;
                let forwarding = pane.forwarding.as_ref()?;
                (old_route.same_saved_route(pane.ssh.as_ref()?)
                    && pane.session.frame().status == Status::Running
                    && forwarding.saved.is_none()
                    && !forwarding.direct.is_closed()
                    && !forwarding.remote.is_closed())
                .then_some(session)
            })
            .min_by_key(|session| session.0)
    }

    pub(in crate::workspace) fn close_pane_tunnels(
        &mut self,
        window: window::Id,
        source: SessionId,
    ) -> Task<WorkspaceMessage> {
        if self
            .windows
            .get(&window)
            .and_then(|state| state.tab())
            .and_then(|tab| tab.tunnel_owner)
            != Some(source)
        {
            self.stop_session(source);
            return Task::none();
        }
        let destination = self.tunnel_successor(window, source);
        if let Some(tab) = self.active_tab_mut(window) {
            tab.tunnel_owner = destination;
        }
        let Some(destination) = destination else {
            self.stop_session(source);
            return Task::none();
        };
        let pane = self.take_session(source).expect("verified tunnel owner");
        let mut forwarding = pane.forwarding.expect("verified tunnel owner transport");
        let mut saved = forwarding.saved.take().expect("verified saved tunnels");
        saved.reassign(destination);
        self.sessions
            .get_mut(&destination)
            .and_then(|pane| pane.forwarding.as_mut())
            .expect("verified live tunnel successor")
            .saved = Some(saved);
        self.remap_tunnel_predecessors(source, destination);
        let listeners = forwarding
            .active
            .into_values()
            .map(|entry| entry.listener)
            .collect();
        self.retire_ssh_session(source, destination, pane.session, listeners)
    }

    pub(in crate::workspace) fn drain_replaced_ssh(
        &mut self,
        source: SessionId,
        destination: SessionId,
    ) -> Task<WorkspaceMessage> {
        self.cancel_split_source(source);
        let Some(pane) = self.take_session(source) else {
            return Task::none();
        };
        let listeners = pane
            .forwarding
            .into_iter()
            .flat_map(|forwarding| forwarding.active.into_values())
            .map(|entry| entry.listener)
            .collect();
        self.retire_ssh_session(source, destination, pane.session, listeners)
    }

    fn retire_ssh_session(
        &mut self,
        source: SessionId,
        destination: SessionId,
        session: Session,
        listeners: Vec<Listener>,
    ) -> Task<WorkspaceMessage> {
        for listener in &listeners {
            listener.stop();
        }
        // Keep the old SSH connection open until its remote listeners finish cancelling.
        self.closing_tunnel_owners.insert(
            source,
            ClosingOwner {
                transport: ClosingTransport::Draining {
                    session: Box::new(session),
                    listeners,
                },
                destination,
            },
        );
        self.tunnel_listener_changed(source)
    }

    pub(in crate::workspace) fn remap_tunnel_predecessors(
        &mut self,
        from: SessionId,
        to: SessionId,
    ) {
        for owner in self
            .closing_tunnel_owners
            .values_mut()
            .filter(|owner| owner.destination == from)
        {
            owner.destination = to;
        }
    }

    pub(in crate::workspace) fn stop_tunnel_predecessors(&mut self, session: SessionId) {
        self.pending_shutdown.extend(
            self.closing_tunnel_owners
                .extract_if(|_, owner| owner.destination == session)
                .map(|(_, owner)| owner.stop()),
        );
    }

    pub(in crate::workspace) fn tunnel_listener_changed(
        &mut self,
        source: SessionId,
    ) -> Task<WorkspaceMessage> {
        if let Some(owner) = self.closing_tunnel_owners.get_mut(&source) {
            if let Some(task) = owner.wait_for_shutdown(source) {
                return task;
            }
            let destination = owner.destination;
            self.closing_tunnel_owners.remove(&source);
            return self.advance_saved_tunnels(destination);
        }
        self.advance_saved_tunnels(source)
    }
}
