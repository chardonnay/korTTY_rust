//! Inactivity is measured with a monotonic clock and never consumes queued work.

use std::time::{Duration, Instant};

use super::SftpWindow;

#[derive(Default)]
pub(super) struct Idle {
    timeout: Option<Duration>,
    deadline: Option<Instant>,
}

impl Idle {
    fn configure(&mut self, preferred: Option<u16>, project: Option<u16>, now: Instant) {
        let timeout = project
            .or(preferred)
            .filter(|minutes| {
                (1..=crate::profile::RemoteProjectTool::MAX_IDLE_MINUTES).contains(minutes)
            })
            .map(|minutes| Duration::from_secs(u64::from(minutes) * 60));
        if self.timeout != timeout {
            self.timeout = timeout;
            self.activity(now);
        }
    }

    fn activity(&mut self, now: Instant) {
        self.deadline = self.timeout.map(|timeout| now + timeout);
    }

    fn expired(&mut self, now: Instant, protected: bool) -> bool {
        if protected {
            self.activity(now);
            return false;
        }
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            self.deadline = None;
            return true;
        }
        false
    }
}

impl SftpWindow {
    pub(crate) fn configure_idle_timeout(&mut self, preferred: Option<u16>, project: Option<u16>) {
        self.idle.configure(preferred, project, Instant::now());
    }

    pub(crate) fn note_activity(&mut self, now: Instant) {
        self.idle.activity(now);
    }

    pub(crate) fn idle_enabled(&self) -> bool {
        self.worker.is_some() && self.idle.deadline.is_some()
    }

    pub(super) fn idle_label(&self, now: Instant) -> Option<String> {
        if !self.idle_enabled() {
            return None;
        }
        let remaining = self.idle.deadline?.saturating_duration_since(now);
        let seconds = remaining.as_secs() + u64::from(remaining.subsec_nanos() != 0);
        Some(format!(
            "Closes when idle in {}:{:02}",
            seconds / 60,
            seconds % 60
        ))
    }

    pub(crate) fn idle_expired(&mut self, now: Instant, live_editor: bool) -> bool {
        if !self.idle_enabled() {
            return false;
        }
        let protected = self.idle_protected(live_editor);
        self.idle.expired(now, protected)
    }

    fn idle_protected(&self, live_editor: bool) -> bool {
        live_editor
            || self.transport_busy()
            || self.workbench.waiting() != 0
            || !self.workbench.active_ids().is_empty()
            || self.external.active()
            || self.prompt.is_some()
            || self.comparison.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sftp_idle_uses_elapsed_time_and_expires_once_until_activity() {
        let start = Instant::now();
        let mut idle = Idle::default();
        assert!(!idle.expired(start + Duration::from_secs(100_000), false));
        idle.configure(Some(1), None, start);
        // Reapplying settings on each tick must not keep an unused window alive.
        idle.configure(Some(1), None, start + Duration::from_secs(59));
        assert!(!idle.expired(start + Duration::from_secs(59), false));
        assert!(idle.expired(start + Duration::from_secs(60), false));
        assert!(!idle.expired(start + Duration::from_secs(61), false));
        idle.activity(start + Duration::from_secs(70));
        assert!(!idle.expired(start + Duration::from_secs(129), false));
        assert!(idle.expired(start + Duration::from_secs(130), false));
    }

    #[test]
    fn sftp_idle_protection_and_project_override_start_a_full_idle_interval() {
        let start = Instant::now();
        let mut idle = Idle::default();
        idle.configure(Some(1), Some(2), start);
        assert!(!idle.expired(start + Duration::from_secs(119), false));
        assert!(!idle.expired(start + Duration::from_secs(180), true));
        assert!(!idle.expired(start + Duration::from_secs(299), false));
        assert!(idle.expired(start + Duration::from_secs(300), false));
        idle.configure(Some(1), None, start + Duration::from_secs(310));
        assert!(idle.expired(start + Duration::from_secs(370), false));
        idle.configure(None, None, start);
        assert!(!idle.expired(start + Duration::from_secs(100_000), false));
        for invalid in [0, 121, u16::MAX] {
            idle.configure(Some(invalid), None, start);
            assert!(idle.timeout.is_none());
        }
    }

    #[test]
    fn sftp_idle_protects_paused_and_running_queue_rows_live_editors_and_prompts() {
        use crate::sftp_window::{
            ConflictDefault, EntryKind, EntryName, Location, Prompt, RemoteEditorPolicy, workbench,
            worker,
        };

        let target = serde_json::from_value(serde_json::json!({
            "host": "idle.example", "port": 22, "username": "fixture"
        }))
        .unwrap();
        let mut state = SftpWindow::new(target, None, true, RemoteEditorPolicy::Allow);
        assert!(!state.idle_protected(false));
        state.workbench.append(
            std::collections::VecDeque::from([worker::Transfer {
                source: Location::Remote(b"/fixture/file".to_vec()),
                target_directory: Location::Local(std::path::PathBuf::from("fixture")),
                name: EntryName::Remote(b"file".to_vec()),
                kind: EntryKind::File,
                title: "Queued fixture".into(),
                size: Some(1),
            }]),
            ConflictDefault::Ask,
        );
        state.workbench.set_paused(true);
        assert!(state.workbench.next_at(1).is_none());
        assert!(!state.transport_busy());
        assert!(state.idle_protected(false));
        state.workbench.set_paused(false);
        let (id, workbench::Pending::Copy(transfer)) = state.workbench.next_at(1).unwrap() else {
            panic!("expected a fresh copy");
        };
        assert!(state.idle_protected(false));
        state
            .workbench
            .finish(id, workbench::Outcome::Completed("Done".into()));
        assert!(!state.idle_protected(false));
        assert!(state.idle_protected(true));
        state.prompt = Some(Prompt::Close(None));
        assert!(state.idle_protected(false));
        state.prompt = None;
        assert!(!state.idle_protected(false));

        state.prompt = Some(Prompt::Close(Some(Box::new(Prompt::Conflict {
            row: id,
            ticket: 0,
            conflict: worker::Conflict {
                transfer,
                existing: worker::Stamp::Remote(Default::default()),
                can_replace: false,
            },
            apply_all: false,
        }))));
        state.revalidate_file_transfers(false);
        state.dismiss_close();
        assert!(!state.idle_protected(false));
    }
}
