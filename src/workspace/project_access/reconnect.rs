//! Schedules reconnects for the exact panes restored by a confirmed project open.

use std::collections::VecDeque;

use iced::widget::{button, row, text};
use iced::{Alignment, Element, Task, window};

use super::attention::{Attention, WaitReason};
use super::{Workspace, WorkspaceMessage};
use crate::workspace::reconnect::ReconnectTarget;

const MAX_PARALLEL: usize = 4;

#[derive(Default)]
pub(in crate::workspace) struct ProjectReconnects {
    waiting: VecDeque<Attempt>,
    active: Vec<Attempt>,
    pub(super) attention: Attention,
}

#[derive(Clone, Copy)]
struct Attempt {
    owner: window::Id,
    target: ReconnectTarget,
}

impl Workspace {
    pub(super) fn queue_project_reconnects(&mut self, owners: &[window::Id]) {
        let mut attempts = Vec::new();
        for &owner in owners {
            let Some(window) = self.windows.get(&owner) else {
                continue;
            };
            for tab in window.terminal_tabs() {
                for (_, &source) in tab.grid.iter() {
                    if self
                        .sessions
                        .get(&source)
                        .is_some_and(|pane| pane.ssh.is_some())
                    {
                        attempts.push(Attempt {
                            owner,
                            target: ReconnectTarget {
                                tab: tab.id,
                                source,
                            },
                        });
                    }
                }
            }
        }
        attempts.sort_unstable_by_key(|attempt| attempt.target.source.0);
        self.project_reconnects
            .waiting
            .retain(|attempt| !owners.contains(&attempt.owner));
        self.project_reconnects.waiting.extend(attempts);
    }

    pub(super) fn cancel_project_reconnects(&mut self, owner: window::Id) {
        self.defer_interrupted_project_reconnects(owner, None);
        if self.project_ssh_target(owner).is_some()
            && let Some(window) = self.windows.get_mut(&owner)
        {
            window.pending_ssh = None;
        }
        let mut active = std::mem::take(&mut self.project_reconnects.active);
        active.retain(|attempt| {
            if attempt.owner != owner {
                return true;
            }
            self.cancel_reconnect_worker_for_target(owner, attempt.target);
            false
        });
        self.project_reconnects.active = active;
    }

    pub(in crate::workspace) fn defer_interrupted_project_reconnects(
        &mut self,
        owner: window::Id,
        tab: Option<u64>,
    ) {
        let belongs = |attempt: &Attempt| {
            attempt.owner == owner && tab.is_none_or(|tab| attempt.target.tab == tab)
        };
        let targets: Vec<_> = self
            .project_reconnects
            .waiting
            .iter()
            .chain(&self.project_reconnects.active)
            .filter(|attempt| belongs(attempt))
            .map(|attempt| attempt.target)
            .collect();
        self.project_reconnects
            .waiting
            .retain(|attempt| !belongs(attempt));
        for target in targets {
            self.defer_project_ssh(owner, target, WaitReason::Canceled);
        }
    }

    pub(in crate::workspace) fn project_candidate_failed(
        &mut self,
        owner: window::Id,
        target: ReconnectTarget,
    ) {
        if self
            .project_reconnects
            .active
            .iter()
            .any(|attempt| attempt.owner == owner && attempt.target == target)
        {
            self.defer_project_ssh(owner, target, WaitReason::ConnectionFailed);
        }
    }

    pub(in crate::workspace) fn project_reconnect_banner(
        &self,
        owner: window::Id,
    ) -> Option<Element<'_, WorkspaceMessage>> {
        let queue = &self.project_reconnects;
        if !queue
            .active
            .iter()
            .chain(&queue.waiting)
            .any(|attempt| attempt.owner == owner)
        {
            return None;
        }
        Some(
            row![
                text("Reconnecting saved project SSH sessions…").size(13),
                button("Cancel reconnects")
                    .on_press(super::wrap(owner, super::Message::CancelReconnects)),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .into(),
        )
    }

    pub(in crate::workspace) fn advance_project_reconnects(&mut self) -> Task<WorkspaceMessage> {
        if self.exiting || self.scheduler_draining {
            self.project_reconnects = ProjectReconnects::default();
            return Task::none();
        }
        self.retry_unlocked_project_sessions();
        if self.project_reconnects.waiting.is_empty() && self.project_reconnects.active.is_empty() {
            return Task::none();
        }
        let mut active = std::mem::take(&mut self.project_reconnects.active);
        let mut waiting = std::mem::take(&mut self.project_reconnects.waiting);
        active.retain(|attempt| {
            self.project_ssh_dial_pending(attempt.owner, attempt.target)
                || self.reconnect_worker_pending(attempt.owner, attempt.target)
        });
        let mut tasks = Vec::new();
        if active.len() < MAX_PARALLEL && self.quick_connect.is_none() {
            waiting.retain(|attempt| {
                if self.clipboard.policy.problem.is_some() {
                    self.defer_project_ssh(attempt.owner, attempt.target, WaitReason::Policy);
                    return false;
                }
                self.reconnect_target(attempt.owner, attempt.target)
                    .is_some_and(|(_, pane)| {
                        pane.ssh.is_some() && pane.frame.status != crate::session::Status::Running
                    })
            });
            while active.len() < MAX_PARALLEL {
                let Some(index) = waiting.iter().position(|attempt| {
                    !active.iter().any(|active| active.owner == attempt.owner)
                        && !self.reconnect_worker_pending(attempt.owner, attempt.target)
                        && self.palette_can_open(attempt.owner)
                }) else {
                    break;
                };
                let attempt = waiting.remove(index).expect("queued reconnect");
                let task = self.start_project_ssh_reconnect(attempt.owner, attempt.target);
                if task.units() > 0 {
                    active.push(attempt);
                    tasks.push(task);
                }
            }
        }
        self.project_reconnects.active = active;
        self.project_reconnects.waiting = waiting;
        Task::batch(tasks)
    }

    pub(super) fn requeue_unlocked_project_ssh(
        &mut self,
        owner: window::Id,
        target: ReconnectTarget,
    ) {
        let queue = &mut self.project_reconnects;
        if !queue
            .waiting
            .iter()
            .chain(&queue.active)
            .any(|attempt| attempt.owner == owner && attempt.target == target)
        {
            queue.waiting.push_back(Attempt { owner, target });
        }
    }
}
