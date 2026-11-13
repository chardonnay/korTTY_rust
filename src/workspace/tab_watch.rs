//! Runtime watches follow their terminal tab across windows.

use std::collections::HashMap;
use std::thread;
use std::time::{Duration, Instant};

use iced::futures::{StreamExt as _, stream::BoxStream};
use iced::{Subscription, Task, window};

use crate::session::{OutputSample, SessionId};

use super::{Message, Workspace};

const POLL_INTERVAL: Duration = Duration::from_secs(1);
const MAX_POLL_GAP: Duration = Duration::from_secs(5);
const QUIET_GAP: Duration = Duration::from_secs(10);
const MIRRORED_WINDOW: Duration = Duration::from_secs(3);
const ATTENTION_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Kind {
    Activity,
    Silence,
}

impl Kind {
    pub(super) const ALL: [Self; 2] = [Self::Activity, Self::Silence];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Activity => "Monitor for Activity",
            Self::Silence => "Monitor for Silence",
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Watch {
    activity: bool,
    silence: bool,
}

impl Watch {
    fn any(self) -> bool {
        self.activity || self.silence
    }
}

#[derive(Default)]
pub(super) struct TabWatch {
    enabled: Watch,
    panes: HashMap<SessionId, PaneWatch>,
    attention: Option<Attention>,
    notification: Option<String>,
}

#[derive(Clone, Copy)]
enum Attention {
    Activity,
    Silence(Duration),
}

impl Attention {
    fn kind(self) -> Kind {
        match self {
            Self::Activity => Kind::Activity,
            Self::Silence(_) => Kind::Silence,
        }
    }
}

impl TabWatch {
    pub(super) fn enabled(&self, kind: Kind) -> bool {
        match kind {
            Kind::Activity => self.enabled.activity,
            Kind::Silence => self.enabled.silence,
        }
    }

    fn toggle(&mut self, kind: Kind) {
        match kind {
            Kind::Activity => self.enabled.activity = !self.enabled.activity,
            Kind::Silence => self.enabled.silence = !self.enabled.silence,
        }
        if !self.enabled.any() {
            self.panes.clear();
        }
    }

    pub(super) fn seen(&mut self) {
        self.attention = None;
        self.notification = None;
        for pane in self.panes.values_mut() {
            pane.activity_armed = true;
        }
    }

    pub(super) fn attention_text(&self) -> Option<String> {
        if let Some(text) = &self.notification {
            return Some(text.clone());
        }
        Some(match self.attention? {
            Attention::Activity => {
                "New output arrived while you were not looking at this tab.".into()
            }
            Attention::Silence(duration) => {
                format!("No output in this tab for {} seconds.", duration.as_secs())
            }
        })
    }

    pub(super) fn mark_notification(&mut self, text: &str) {
        self.notification = Some(text.chars().take(200).collect());
    }
}

struct PaneWatch {
    watch: Watch,
    polled: Instant,
    last_output: Option<Instant>,
    activity_armed: bool,
    silence_armed: bool,
}

impl PaneWatch {
    fn new(watch: Watch, last_output: Option<Instant>, now: Instant, silence: Duration) -> Self {
        Self {
            watch,
            polled: now,
            last_output,
            activity_armed: true,
            silence_armed: last_output
                .is_some_and(|last| now.saturating_duration_since(last) < silence),
        }
    }

    fn poll(
        &mut self,
        watch: Watch,
        sample: OutputSample,
        now: Instant,
        silence: Duration,
        seen: bool,
    ) -> Option<Attention> {
        if now.saturating_duration_since(self.polled) > MAX_POLL_GAP {
            *self = Self::new(watch, sample.last, now, silence);
            return None;
        }
        if watch.activity && !self.watch.activity {
            self.activity_armed = true;
        }
        if watch.silence && !self.watch.silence {
            self.silence_armed = self
                .last_output
                .is_some_and(|last| now.saturating_duration_since(last) < silence);
        }
        let mut attention = None;
        if let Some(first) = sample.first {
            let mirrored = sample
                .mirrored
                .is_some_and(|at| now.saturating_duration_since(at) < MIRRORED_WINDOW);
            if !mirrored {
                if watch.activity
                    && self.activity_armed
                    && !seen
                    && self
                        .last_output
                        .is_none_or(|last| first.saturating_duration_since(last) >= QUIET_GAP)
                {
                    attention = Some(Attention::Activity);
                    self.activity_armed = false;
                }
                if watch.silence {
                    self.silence_armed = true;
                }
            }
            self.last_output = sample.last;
        }
        if seen {
            self.activity_armed = true;
        }
        if watch.silence
            && self.silence_armed
            && self
                .last_output
                .is_some_and(|last| now.saturating_duration_since(last) >= silence)
        {
            self.silence_armed = false;
            if !seen {
                attention = Some(Attention::Silence(silence));
            }
        }
        self.watch = watch;
        self.polled = now;
        attention
    }
}

#[derive(Default)]
pub(super) struct AttentionRequests(HashMap<(Option<SessionId>, Kind), Instant>);

impl Workspace {
    pub(super) fn tab_watch_subscription(&self) -> Subscription<Message> {
        if !self.exiting
            && !self.scheduler_draining
            && self
                .windows
                .values()
                .flat_map(|window| window.terminal_tabs())
                .any(|tab| tab.watch.enabled.any())
        {
            Subscription::run(clock)
        } else {
            Subscription::none()
        }
    }

    pub(super) fn toggle_tab_watch(&mut self, owner: window::Id, target: u64, kind: Kind) {
        if self.exiting || self.scheduler_draining || !self.palette_can_open(owner) {
            return;
        }
        if let Some(tab) = self
            .windows
            .get_mut(&owner)
            .and_then(|window| window.terminal_tabs_mut().find(|tab| tab.id == target))
        {
            let was_watched = tab.watch.enabled.any();
            tab.watch.toggle(kind);
            if !was_watched && tab.watch.enabled.any() {
                let now = Instant::now();
                let silence =
                    Duration::from_secs(u64::from(self.window_settings.terminal_silence_seconds));
                for (_, session) in tab.grid.iter() {
                    if let Some(pane) = self.sessions.get(session) {
                        let sample = pane.session.take_output_activity();
                        tab.watch.panes.insert(
                            *session,
                            PaneWatch::new(tab.watch.enabled, sample.last, now, silence),
                        );
                    }
                }
            }
        }
    }

    pub(super) fn poll_tab_watches(&mut self, now: Instant) -> Task<Message> {
        if self.exiting || self.scheduler_draining {
            return Task::none();
        }
        self.tab_attention
            .0
            .retain(|_, at| now.saturating_duration_since(*at) < ATTENTION_INTERVAL);
        let silence = Duration::from_secs(u64::from(self.window_settings.terminal_silence_seconds));
        let mut requests = Vec::new();
        let mut notifications = Vec::new();
        let system_attention = self.clipboard.policy.desktop_notifications_allowed
            && self.clipboard.policy.problem.is_none();
        for (&owner, window) in &mut self.windows {
            let seen = window.active_tab_id().filter(|_| window.focused);
            let mut request = false;
            for tab in window
                .terminal_tabs_mut()
                .filter(|tab| tab.watch.enabled.any())
            {
                let watch = tab.watch.enabled;
                let seen = seen == Some(tab.id);
                tab.watch
                    .panes
                    .retain(|session, _| tab.grid.iter().any(|(_, id)| id == session));
                for (_, &session) in tab.grid.iter() {
                    let Some(pane) = self.sessions.get(&session) else {
                        continue;
                    };
                    let sample = pane.session.take_output_activity();
                    let state = match tab.watch.panes.entry(session) {
                        std::collections::hash_map::Entry::Occupied(state) => state.into_mut(),
                        std::collections::hash_map::Entry::Vacant(entry) => {
                            entry.insert(PaneWatch::new(watch, sample.last, now, silence));
                            continue;
                        }
                    };
                    if let Some(attention) = state.poll(watch, sample, now, silence, seen) {
                        tab.watch.attention = Some(attention);
                        let slot = (!self.multi_exec.contains(session)).then_some(session);
                        if system_attention
                            && let std::collections::hash_map::Entry::Vacant(entry) =
                                self.tab_attention.0.entry((slot, attention.kind()))
                        {
                            entry.insert(now);
                            request = true;
                            notifications.push((
                                session,
                                attention.kind(),
                                match attention {
                                    Attention::Activity => {
                                        "New terminal output is available.".to_owned()
                                    }
                                    Attention::Silence(duration) => format!(
                                        "No terminal output for {} seconds.",
                                        duration.as_secs()
                                    ),
                                },
                            ));
                        }
                    }
                }
            }
            if request {
                requests.push(window::request_user_attention(
                    owner,
                    Some(window::UserAttention::Informational),
                ));
            }
        }
        requests.extend(notifications.into_iter().map(|(session, kind, body)| {
            self.notify_terminal_kind(
                session,
                match kind {
                    Kind::Activity => super::terminal_notifications::Kind::Activity,
                    Kind::Silence => super::terminal_notifications::Kind::Silence,
                },
                "kortty · Terminal monitor",
                &body,
            )
        }));
        Task::batch(requests)
    }
}

fn clock() -> BoxStream<'static, Message> {
    let (send, receive) = async_channel::bounded(1);
    let _ = thread::Builder::new()
        .name("kortty-tab-watch".into())
        .spawn(move || {
            while !send.is_closed() {
                thread::sleep(POLL_INTERVAL);
                let _ = send.try_send(());
            }
        });
    receive.map(|()| Message::TabWatchTick).boxed()
}

#[cfg(test)]
mod tests;
