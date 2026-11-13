//! Swarm notification transitions contain only a bound destination and event kind.

use std::collections::HashMap;

use iced::window;

use crate::workspace::terminal_notifications::AiRunNotice;

use super::{Group, Key, Phase, Queue, Status, Workspace};

#[derive(Default)]
pub(super) struct State {
    round: u64,
    stopped: bool,
    reviews: HashMap<Key, u64>,
    completion_reported: bool,
}

impl State {
    pub(super) fn new(round: u64) -> Self {
        Self {
            round,
            ..Self::default()
        }
    }

    pub(super) fn stop(&mut self) {
        self.stopped = true;
    }

    pub(super) fn reset_target(&mut self, key: Key) {
        self.reviews.remove(&key);
        self.completion_reported = false;
    }
}

impl Group {
    fn notice_current(&self, round: u64) -> bool {
        !self.closing
            && !self.notifications.stopped
            && self.round == round
            && self.notifications.round == round
            && self
                .queue
                .as_ref()
                .is_some_and(|queue| queue.rows().iter().all(|row| !row.target.private))
    }

    pub(super) fn take_notifications(&mut self, workspace: &Workspace) -> Vec<(Key, AiRunNotice)> {
        if !self.notice_current(self.round) {
            return Vec::new();
        }
        let Some(queue) = &self.queue else {
            return Vec::new();
        };
        if queue.complete() {
            if std::mem::replace(&mut self.notifications.completion_reported, true) {
                return Vec::new();
            }
            return completion_kind(queue)
                .zip(self.completion_target(workspace, queue))
                .map(|(kind, key)| (key, kind))
                .into_iter()
                .collect();
        }

        let mut notices = Vec::new();
        for (&member, ticket) in &self.members {
            let Some(run) = workspace.terminal_agents.runs.get(&member) else {
                continue;
            };
            if matches!(run.phase, Phase::ProbeApproval | Phase::Approval)
                && !run.paused
                && !run.cancel.requested()
                && self.notifications.reviews.insert(ticket.key(), run.epoch) != Some(run.epoch)
            {
                notices.push((ticket.key(), AiRunNotice::Approval));
            }
        }
        // The notification limiter keeps the first event of the same kind.
        notices.sort_by_key(|(key, _)| key.stable());
        notices
    }

    fn completion_target(&self, workspace: &Workspace, queue: &Queue) -> Option<Key> {
        let keys = || queue.rows().iter().map(|row| row.target.key);
        if keys().all(|key| key.pane().is_none()) {
            return keys().next();
        }
        keys()
            .filter(|key| {
                key.pane().is_some_and(|(session, stream)| {
                    workspace.agent_pane(session).is_some_and(|(_, pane, tab)| {
                        pane.stream_generation == stream && !tab.incognito
                    })
                })
            })
            .min_by_key(|&key| (self.focused != Some(key), key.stable()))
    }
}

fn completion_kind(queue: &Queue) -> Option<AiRunNotice> {
    if queue
        .rows()
        .iter()
        .any(|row| row.status == Status::Succeeded)
    {
        Some(AiRunNotice::Finished)
    } else if queue
        .rows()
        .iter()
        .any(|row| matches!(row.status, Status::Failed | Status::Blocked))
    {
        Some(AiRunNotice::Stopped)
    } else {
        None
    }
}

impl Workspace {
    pub(in crate::workspace) fn swarm_notice_current(&self, id: window::Id, round: u64) -> bool {
        self.swarm_policy().is_ok()
            && self
                .terminal_agents
                .swarms
                .get(&id)
                .is_some_and(|group| group.notice_current(round))
    }
}
