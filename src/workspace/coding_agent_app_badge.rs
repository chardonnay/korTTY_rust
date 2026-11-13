use std::collections::HashSet;
#[cfg(windows)]
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Instant;

use iced::{Task, window};

use crate::session::{CodingAgentState, SessionId};

use super::{Message, Window, Workspace};

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
struct Waiting {
    session: SessionId,
    instance: u64,
    since: Instant,
}

#[derive(Default)]
pub(super) struct Badge {
    waiting: HashSet<Waiting>,
    count: usize,
    native: Option<bool>,
    #[cfg(target_os = "linux")]
    launcher: Option<kortty_windowing::LauncherBadge>,
    #[cfg(target_os = "linux")]
    launcher_generation: u64,
    #[cfg(target_os = "linux")]
    launcher_urgent: bool,
    #[cfg(windows)]
    generation: Arc<AtomicU64>,
}

impl Badge {
    fn observe(&mut self, waiting: HashSet<Waiting>, enabled: bool) -> (bool, bool) {
        let count = if enabled { waiting.len() } else { 0 };
        let changed = std::mem::replace(&mut self.count, count) != count;
        let urgent = count != 0 && waiting.iter().any(|agent| !self.waiting.contains(agent));
        self.waiting = waiting;
        (changed, urgent)
    }

    pub(super) fn title_count(&self) -> usize {
        // LauncherEntry has no receiver acknowledgement, so retain the title counter on Linux.
        if self.native == Some(true) && !cfg!(target_os = "linux") {
            0
        } else {
            self.count
        }
    }
}

impl Workspace {
    pub(super) fn reconcile_coding_agent_app_badge(
        &mut self,
        opened: Option<window::Id>,
    ) -> Task<Message> {
        let waiting = if self.exiting || !self.window_settings.coding_agent_detection_enabled {
            HashSet::new()
        } else {
            self.windows
                .values()
                .flat_map(Window::terminal_tabs)
                .flat_map(|tab| tab.grid.iter())
                .filter_map(|(_, &session)| {
                    let status = self.coding_agent_status(session)?;
                    (status.state == CodingAgentState::Blocked).then_some(Waiting {
                        session,
                        instance: status.instance,
                        since: status.since,
                    })
                })
                .collect()
        };
        let (changed, urgent) = self
            .coding_agent_app_badge
            .observe(waiting, self.window_settings.coding_agent_app_badge_enabled);
        let attention_allowed = self.clipboard.policy.desktop_notifications_allowed
            && self.clipboard.policy.problem.is_none()
            && !self.windows.values().any(|window| window.focused);
        #[cfg(target_os = "linux")]
        let update = self.apply_linux_coding_agent_badge(changed, urgent, attention_allowed);
        #[cfg(not(target_os = "linux"))]
        let update = self.apply_native_coding_agent_badge(changed, opened);
        #[cfg(target_os = "linux")]
        let _ = opened;
        if !cfg!(target_os = "linux")
            && urgent
            && self.coding_agent_app_badge.native == Some(true)
            && attention_allowed
            && let Some((&owner, _)) = self.windows.iter().find(|(_, window)| {
                window.terminal_tabs().any(|tab| {
                    tab.grid.iter().any(|(_, session)| {
                        self.coding_agent_status(*session)
                            .is_some_and(|status| status.state == CodingAgentState::Blocked)
                    })
                })
            })
        {
            update.chain(window::request_user_attention(
                owner,
                Some(window::UserAttention::Informational),
            ))
        } else {
            update
        }
    }

    #[cfg(target_os = "macos")]
    fn apply_native_coding_agent_badge(
        &mut self,
        changed: bool,
        _: Option<window::Id>,
    ) -> Task<Message> {
        let badge = &mut self.coding_agent_app_badge;
        if badge.native.is_none() || changed && badge.native == Some(true) {
            badge.native = Some(kortty_macos::set_dock_badge(badge.count));
        }
        Task::none()
    }

    #[cfg(windows)]
    fn apply_native_coding_agent_badge(
        &mut self,
        changed: bool,
        opened: Option<window::Id>,
    ) -> Task<Message> {
        let badge = &mut self.coding_agent_app_badge;
        if badge.native == Some(false) {
            return Task::none();
        }
        let opened = opened.filter(|id| self.windows.contains_key(id));
        if !changed && opened.is_none() {
            return Task::none();
        }
        badge.native = Some(true);
        if changed {
            let generation = badge
                .generation
                .fetch_add(1, Ordering::Relaxed)
                .wrapping_add(1);
            Task::batch(
                self.windows
                    .keys()
                    .map(|&id| windows_update(id, badge, generation, false)),
            )
        } else {
            opened.map_or_else(Task::none, |id| {
                windows_update(id, badge, badge.generation.load(Ordering::Relaxed), false)
            })
        }
    }

    #[cfg(windows)]
    pub(super) fn finish_coding_agent_badge_icon(
        &mut self,
        id: window::Id,
        generation: u64,
        retry: bool,
        applied: bool,
    ) -> Task<Message> {
        let badge = &mut self.coding_agent_app_badge;
        if applied
            || badge.native != Some(true)
            || badge.generation.load(Ordering::Relaxed) != generation
            || !self.windows.contains_key(&id)
        {
            return Task::none();
        }
        if !retry {
            return windows_update(id, badge, generation, true);
        }
        badge.native = Some(false);
        let generation = badge
            .generation
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        Task::batch(self.windows.keys().map(|&id| {
            let current = Arc::clone(&badge.generation);
            super::coding_agent_app_icon::apply_windows_icon(id, 0, move || {
                current.load(Ordering::Relaxed) == generation
            })
            .discard()
        }))
    }

    #[cfg(target_os = "linux")]
    fn apply_linux_coding_agent_badge(
        &mut self,
        changed: bool,
        urgent: bool,
        attention_allowed: bool,
    ) -> Task<Message> {
        let badge = &mut self.coding_agent_app_badge;
        let urgent = attention_allowed && badge.count != 0 && (urgent || badge.launcher_urgent);
        let urgency_changed = std::mem::replace(&mut badge.launcher_urgent, urgent) != urgent;
        if !changed && !urgency_changed {
            return Task::none();
        }
        if badge.launcher.is_none() && badge.count != 0 {
            badge.launcher = kortty_windowing::LauncherBadge::start();
        }
        let Some(launcher) = &badge.launcher else {
            return Task::none();
        };
        badge.launcher_generation = badge.launcher_generation.wrapping_add(1);
        let generation = badge.launcher_generation;
        let (send, receive) = async_channel::bounded(1);
        launcher.set(badge.count, urgent, move |applied| {
            let _ = send.try_send(applied);
        });
        Task::perform(async move { receive.recv().await.ok() }, move |applied| {
            applied.map_or(Message::Noop, |applied| {
                Message::CodingAgentLauncherBadgeResult {
                    generation,
                    applied,
                }
            })
        })
    }

    #[cfg(target_os = "linux")]
    pub(super) fn finish_coding_agent_launcher_badge(&mut self, generation: u64, applied: bool) {
        let badge = &mut self.coding_agent_app_badge;
        if generation == badge.launcher_generation && !applied {
            badge.launcher = None;
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    fn apply_native_coding_agent_badge(&mut self, _: bool, _: Option<window::Id>) -> Task<Message> {
        Task::none()
    }
}

#[cfg(windows)]
fn windows_update(id: window::Id, badge: &Badge, generation: u64, retry: bool) -> Task<Message> {
    let current = Arc::clone(&badge.generation);
    super::coding_agent_app_icon::apply_windows_icon(id, badge.count, move || {
        current.load(Ordering::Relaxed) == generation
    })
    .map(move |applied| Message::CodingAgentBadgeIconResult {
        id,
        generation,
        retry,
        applied,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coding_agent_app_badge_counts_transitions_and_clears_on_disable() {
        let mut badge = Badge {
            native: Some(false),
            ..Badge::default()
        };
        let waiting = Waiting {
            session: SessionId(1),
            instance: 7,
            since: Instant::now(),
        };
        let replacement = Waiting {
            instance: 8,
            ..waiting
        };
        assert_eq!(badge.observe(HashSet::from([waiting]), true), (true, true));
        assert_eq!(badge.title_count(), 1);
        assert_eq!(
            badge.observe(HashSet::from([waiting]), true),
            (false, false)
        );
        assert_eq!(
            badge.observe(HashSet::from([replacement]), true),
            (false, true)
        );
        assert_eq!(
            badge.observe(HashSet::from([replacement]), false),
            (true, false)
        );
        assert_eq!(badge.title_count(), 0);
        assert_eq!(
            badge.observe(HashSet::from([replacement]), true),
            (true, false)
        );
        badge.native = Some(true);
        assert_eq!(badge.title_count(), usize::from(cfg!(target_os = "linux")));
        assert_eq!(badge.observe(HashSet::new(), true), (true, false));
        assert_eq!(badge.count, 0);
    }
}
