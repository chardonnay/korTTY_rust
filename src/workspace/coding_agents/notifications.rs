use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use iced::futures::{StreamExt as _, stream::BoxStream};
use iced::{Subscription, Task, window};

use crate::session::{CodingAgentState, CodingAgentStatus, SessionId, Status};
use crate::workspace::{Message, Workspace, control_access};

use super::{FocusTarget, label, seen_pane};

const SETTLE: Duration = Duration::from_millis(800);
const MIN_INTERVAL: Duration = Duration::from_secs(10);
const TICK: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Kind {
    Blocked,
    Finished,
}

impl Kind {
    fn current(effective: CodingAgentState, observed: CodingAgentState) -> Option<Self> {
        use CodingAgentState::{Blocked, Done, Idle, Unknown};

        match (effective, observed) {
            (Blocked, Blocked) => Some(Self::Blocked),
            (Done, Idle | Unknown) => Some(Self::Finished),
            _ => None,
        }
    }

    fn suffix(self) -> &'static str {
        match self {
            Self::Blocked => "needs a decision",
            Self::Finished => "finished",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Pending {
    target: FocusTarget,
    kind: Kind,
    since: Instant,
    due: Instant,
}

#[derive(Default)]
struct PaneNotice {
    pending: Option<Pending>,
    delivered: Option<Instant>,
}

impl PaneNotice {
    fn resolve(&mut self, now: Instant, unseen_and_current: bool) -> bool {
        if !unseen_and_current {
            self.pending = None;
            return false;
        }
        if self
            .delivered
            .is_some_and(|at| now.saturating_duration_since(at) < MIN_INTERVAL)
        {
            return false;
        }
        self.pending = None;
        self.delivered = Some(now);
        true
    }
}

pub(in crate::workspace) struct Coordinator {
    panes: HashMap<SessionId, PaneNotice>,
    generation: Arc<AtomicU64>,
    sender: async_channel::Sender<Activation>,
    receiver: async_channel::Receiver<Activation>,
}

impl Default for Coordinator {
    fn default() -> Self {
        let (sender, receiver) = async_channel::bounded(32);
        Self {
            panes: HashMap::new(),
            generation: Arc::default(),
            sender,
            receiver,
        }
    }
}

impl Coordinator {
    fn observe(
        &mut self,
        target: FocusTarget,
        before: Option<CodingAgentStatus>,
        after: CodingAgentStatus,
        raw: CodingAgentStatus,
        now: Instant,
    ) {
        let kind = Kind::current(after.state, raw.state);
        let Some(kind) = kind.filter(|_| raw.instance == after.instance) else {
            if let Some(pane) = self.panes.get_mut(&target.session) {
                pane.pending = None;
            }
            return;
        };
        let entered = match kind {
            Kind::Blocked => before.is_none_or(|before| before != after),
            Kind::Finished => before.is_some_and(|before| {
                before.instance == after.instance && before.state == CodingAgentState::Working
            }),
        };
        if entered {
            self.panes.entry(target.session).or_default().pending = Some(Pending {
                target,
                kind,
                since: after.since,
                due: now
                    + match kind {
                        Kind::Blocked => SETTLE,
                        Kind::Finished => Duration::ZERO,
                    },
            });
        }
    }

    pub(in crate::workspace) fn retain_sessions(
        &mut self,
        mut alive: impl FnMut(SessionId) -> bool,
    ) {
        self.panes.retain(|&id, _| alive(id));
    }

    fn settled(&self, now: Instant) -> Vec<Pending> {
        let mut pending: Vec<_> = self
            .panes
            .values()
            .filter_map(|pane| pane.pending.filter(|pending| pending.due <= now))
            .collect();
        pending.sort_unstable_by_key(|pending| (pending.due, pending.target.session.0));
        pending
    }
}

#[derive(Clone)]
pub struct Activation {
    target: FocusTarget,
    generation: u64,
    token: Option<String>,
}

impl std::fmt::Debug for Activation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Coding agent desktop notification activation")
    }
}

struct ActivationFeed {
    generation: Arc<AtomicU64>,
    receiver: async_channel::Receiver<Activation>,
}

impl Hash for ActivationFeed {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.generation).hash(state);
    }
}

fn activations(feed: &ActivationFeed) -> BoxStream<'static, Message> {
    feed.receiver
        .clone()
        .map(Message::CodingAgentNotificationActivated)
        .boxed()
}

fn clock() -> BoxStream<'static, Message> {
    let (sender, receiver) = async_channel::bounded(1);
    let _ = std::thread::Builder::new()
        .name("kortty-coding-agent-notifications".into())
        .spawn(move || {
            while !sender.is_closed() {
                std::thread::sleep(TICK);
                let _ = sender.try_send(());
            }
        });
    receiver
        .map(|()| Message::CodingAgentNotificationsTick)
        .boxed()
}

impl Workspace {
    pub(in crate::workspace) fn coding_agent_notifications_enabled(&self) -> bool {
        !self.exiting
            && !self.scheduler_draining
            && self.window_settings.coding_agent_detection_enabled
            && self.window_settings.coding_agent_notifications_enabled
            && self.clipboard.policy.desktop_notifications_allowed
            && self.clipboard.policy.problem.is_none()
    }

    pub(in crate::workspace) fn coding_agent_notification_subscription(
        &self,
    ) -> Subscription<Message> {
        if !self.coding_agent_notifications_enabled() {
            return Subscription::none();
        }
        let coordinator = &self.coding_agent_notifications;
        let activation = Subscription::run_with(
            ActivationFeed {
                generation: Arc::clone(&coordinator.generation),
                receiver: coordinator.receiver.clone(),
            },
            activations,
        );
        if coordinator
            .panes
            .values()
            .any(|pane| pane.pending.is_some())
        {
            Subscription::batch([activation, Subscription::run(clock)])
        } else {
            activation
        }
    }

    pub(in crate::workspace) fn coding_agent_notification_permission_changed(
        &mut self,
    ) -> Task<Message> {
        let enabled = self.coding_agent_notifications_enabled();
        #[cfg(windows)]
        let shutdown_native = !self.terminal_notifications_enabled();
        let coordinator = &mut self.coding_agent_notifications;
        let _generation = coordinator
            .generation
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        if !enabled {
            for pane in coordinator.panes.values_mut() {
                pane.pending = None;
            }
            #[cfg(windows)]
            if shutdown_native && let Some(&owner) = self.windows.keys().next() {
                let gate = Arc::clone(&coordinator.generation);
                return window::run(owner, move |_| {
                    if gate.load(Ordering::Relaxed) == _generation {
                        kortty_windowing::shutdown_desktop_notifications();
                    }
                })
                .discard();
            }
        }
        Task::none()
    }

    pub(in crate::workspace) fn observe_coding_agent_notification(
        &mut self,
        session: SessionId,
        before: Option<CodingAgentStatus>,
    ) {
        if !self.coding_agent_notifications_enabled() {
            return;
        }
        let Some((after, raw, stream_generation)) = self.sessions.get(&session).and_then(|pane| {
            (pane.frame.status == Status::Running).then_some(())?;
            Some((
                pane.coding_agent.status?,
                pane.frame.coding_agent()?,
                pane.stream_generation,
            ))
        }) else {
            self.coding_agent_notifications.panes.remove(&session);
            return;
        };
        self.coding_agent_notifications.observe(
            FocusTarget::new(session, stream_generation, after.instance),
            before,
            after,
            raw,
            Instant::now(),
        );
    }

    pub(in crate::workspace) fn poll_coding_agent_notifications(&mut self) -> Task<Message> {
        if !self.coding_agent_notifications_enabled() {
            return Task::none();
        }
        let now = Instant::now();
        let seen = seen_pane(&self.windows);
        let pending = self.coding_agent_notifications.settled(now);
        let mut tasks = Vec::new();
        for event in pending {
            let id = event.target.session;
            let location = (seen != Some(id))
                .then(|| self.notification_location(event))
                .flatten();
            if !self
                .coding_agent_notifications
                .panes
                .get_mut(&id)
                .is_some_and(|pane| pane.resolve(now, location.is_some()))
            {
                continue;
            }
            let Some((owner, tab, frame)) = location else {
                continue;
            };
            let Some((title, body)) = self.notification_text(event, owner, tab, &frame) else {
                continue;
            };
            let coordinator = &self.coding_agent_notifications;
            tasks.push(send(
                owner,
                title,
                body,
                event.target,
                Arc::clone(&coordinator.generation),
                coordinator.sender.clone(),
            ));
        }
        Task::batch(tasks)
    }

    fn notification_location(
        &self,
        event: Pending,
    ) -> Option<(window::Id, usize, Arc<crate::session::Frame>)> {
        let id = event.target.session;
        let pane = self.sessions.get(&id)?;
        let frame = pane.session.frame();
        let raw = frame.coding_agent()?;
        let status = pane.coding_agent.status?;
        if frame.status != Status::Running
            || pane.stream_generation != event.target.stream_generation
            || status.instance != event.target.instance
            || raw.instance != event.target.instance
            || status.since != event.since
            || Kind::current(status.state, raw.state) != Some(event.kind)
        {
            return None;
        }
        self.windows.iter().find_map(|(&owner, window)| {
            let (index, _) = window.pane_location(id)?;
            let tab = window.tabs.get(index)?.terminal()?;
            (!tab.incognito).then_some((owner, index, Arc::clone(&frame)))
        })
    }

    fn notification_text(
        &self,
        event: Pending,
        owner: window::Id,
        index: usize,
        frame: &crate::session::Frame,
    ) -> Option<(String, String)> {
        let tab = self.windows.get(&owner)?.tabs.get(index)?.terminal()?;
        let pane = self.sessions.get(&event.target.session)?;
        let raw = frame.coding_agent()?;
        let policy = &self.clipboard.policy.terminal_logging;
        let name = pane.coding_agent.alias.as_deref().unwrap_or(raw.name);
        let name = pane
            .session
            .redact_journal_note(name, policy)
            .map(|name| label(&name, 42))
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| raw.name.to_owned());
        let title = format!("{name} {}", event.kind.suffix());
        let mut location = self.tab_names(tab).0;
        if tab.grid.len() > 1
            && let Some(position) = control_access::ordered_panes(tab)
                .iter()
                .position(|(_, session)| *session == event.target.session)
        {
            location.push_str(&format!(" › Pane {}", position + 1));
        }
        if let Some(directory) = &frame.shell_activity().directory {
            location.push_str(" › ");
            location.push_str(&directory.path);
        }
        let mut body = pane
            .session
            .redact_journal_note(&location, policy)
            .map_or_else(String::new, |location| label(&location, 140));
        if let Some(evidence) = frame
            .coding_agent_details()
            .and_then(|details| details.rule.as_ref())
            .and_then(|rule| pane.session.redact_journal_note(&rule.evidence, policy))
            .map(|evidence| label(&evidence, 100))
            .filter(|evidence| !evidence.is_empty())
        {
            if !body.is_empty() {
                body.push('\n');
            }
            body.push_str(&evidence);
        }
        Some((title, body))
    }

    pub(in crate::workspace) fn activate_coding_agent_notification(
        &mut self,
        activation: Activation,
    ) -> Task<Message> {
        if !self.coding_agent_notifications_enabled()
            || self
                .coding_agent_notifications
                .generation
                .load(Ordering::Relaxed)
                != activation.generation
        {
            return Task::none();
        }
        self.focus_coding_agent_with(activation.target, move |owner| {
            raise_window(owner, activation.token)
        })
    }
}

fn send(
    _owner: window::Id,
    title: String,
    body: String,
    target: FocusTarget,
    gate: Arc<AtomicU64>,
    sender: async_channel::Sender<Activation>,
) -> Task<Message> {
    let generation = gate.load(Ordering::Relaxed);
    let callback_gate = Arc::clone(&gate);
    let on_activate = move |token| {
        if callback_gate.load(Ordering::Relaxed) == generation {
            let _ = sender.try_send(Activation {
                target,
                generation,
                token,
            });
        }
    };
    crate::workspace::terminal_notifications::send_native(
        Some(_owner),
        title,
        body,
        gate,
        on_activate,
    )
}

fn raise_window(owner: window::Id, token: Option<String>) -> Task<Message> {
    crate::workspace::terminal_notifications::raise_window(owner, token)
}
#[cfg(test)]
mod tests {
    use super::*;
    use CodingAgentState::{Blocked, Done, Idle, Working};

    fn status(state: CodingAgentState, since: Instant) -> CodingAgentStatus {
        CodingAgentStatus {
            id: "gemini-cli",
            name: "Gemini CLI",
            state,
            since,
            instance: 7,
        }
    }

    #[test]
    fn coding_agent_notifications_wait_drop_stale_and_rate_limit_each_pane() {
        let start = Instant::now();
        let id = SessionId(1);
        let target = FocusTarget::new(id, 1, 7);
        let blocked = status(Blocked, start);
        let working = status(Working, start + SETTLE / 2);
        let mut coordinator = Coordinator::default();
        coordinator.observe(target, None, blocked, blocked, start);
        assert!(coordinator.settled(start + SETTLE / 2).is_empty());
        coordinator.observe(target, Some(blocked), working, working, working.since);
        assert!(coordinator.settled(start + SETTLE).is_empty());

        let blocked = status(Blocked, start + SETTLE);
        let delivered = blocked.since + SETTLE;
        coordinator.observe(target, Some(working), blocked, blocked, blocked.since);
        assert_eq!(coordinator.settled(delivered).len(), 1);
        assert!(
            coordinator
                .panes
                .get_mut(&id)
                .unwrap()
                .resolve(delivered, true)
        );
        coordinator.observe(target, Some(blocked), blocked, blocked, delivered);
        assert!(coordinator.settled(delivered + MIN_INTERVAL).is_empty());

        let again = status(Blocked, delivered + SETTLE);
        coordinator.observe(target, Some(working), again, again, again.since);
        let due = again.since + SETTLE;
        let other = FocusTarget::new(SessionId(2), 1, 7);
        coordinator.observe(other, None, again, again, again.since);
        assert_eq!(coordinator.settled(due).len(), 2);
        assert!(!coordinator.panes.get_mut(&id).unwrap().resolve(due, true));
        assert!(
            coordinator
                .panes
                .get_mut(&other.session)
                .unwrap()
                .resolve(due, true)
        );
        assert!(
            coordinator
                .panes
                .get_mut(&id)
                .unwrap()
                .resolve(delivered + MIN_INTERVAL, true)
        );
    }

    #[test]
    fn coding_agent_notifications_only_queue_unseen_synthetic_completion() {
        let now = Instant::now();
        let target = FocusTarget::new(SessionId(1), 1, 7);
        let working = status(Working, now - SETTLE);
        let blocked = status(Blocked, now - SETTLE);
        let done = status(Done, now);
        let idle = status(Idle, now);
        let mut coordinator = Coordinator::default();
        coordinator.observe(target, Some(working), done, done, now);
        assert!(coordinator.settled(now).is_empty());
        coordinator.observe(target, Some(blocked), idle, idle, now);
        assert!(coordinator.settled(now).is_empty());
        coordinator.observe(target, Some(working), done, idle, now);
        let pending = coordinator.settled(now);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].kind, Kind::Finished);
        assert!(
            !coordinator
                .panes
                .get_mut(&target.session)
                .unwrap()
                .resolve(now, false)
        );
        assert!(coordinator.settled(now + MIN_INTERVAL).is_empty());
    }
}
