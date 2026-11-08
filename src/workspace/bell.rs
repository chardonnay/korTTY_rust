use std::time::{Duration, Instant};

use iced::{Task, window};

use super::{Message, Workspace};
use crate::session::SessionId;

const MIN_SOUND_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Default)]
pub(super) struct Bell {
    pub(super) enabled: bool,
    last_request: Option<Instant>,
}

impl Bell {
    fn allow(&mut self, events: u64, now: Instant) -> bool {
        if !self.enabled
            || events == 0
            || self.last_request.is_some_and(|previous| {
                now.saturating_duration_since(previous) < MIN_SOUND_INTERVAL
            })
        {
            return false;
        }
        self.last_request = Some(now);
        true
    }
}

impl Workspace {
    pub(super) fn play_bell(&mut self, session: SessionId, events: u64) -> Task<Message> {
        if !self.bell.allow(events, Instant::now()) {
            return Task::none();
        }
        let Some((&id, _)) = self
            .windows
            .iter()
            .find(|(_, window)| window.pane_location(session).is_some())
        else {
            return Task::none();
        };
        window::run(id, |native| {
            if let Ok(display) = native.display_handle() {
                kortty_windowing::request_system_bell(display);
            }
        })
        .discard()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sound_is_opt_in_and_coalesces_bursts_without_a_backlog() {
        let mut bell = Bell::default();
        let now = Instant::now();
        assert!(!bell.allow(1, now));
        bell.enabled = true;
        assert!(!bell.allow(0, now));
        assert!(bell.allow(10_000, now));
        assert!(!bell.allow(1, now + Duration::from_millis(1)));
        assert!(!bell.allow(0, now + MIN_SOUND_INTERVAL));
        assert!(bell.allow(1, now + MIN_SOUND_INTERVAL));
        bell.enabled = false;
        assert!(!bell.allow(1, now + MIN_SOUND_INTERVAL * 2));
    }
}
