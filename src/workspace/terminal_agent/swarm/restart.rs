//! Restarts drain the current attempt before allocating a new reviewed run.

use std::collections::HashMap;

use iced::window;

use super::{Choice, Group, Key, Launch, ProfileChoice, Status, SwarmMode, Ticket, Workspace};

#[derive(Default)]
pub(super) struct State {
    last: Option<Round>,
    round: bool,
    targets: HashMap<Key, Ticket>,
}

pub(super) struct Round {
    round: u64,
    mode: SwarmMode,
    profile: ProfileChoice,
    goal: String,
    choices: Vec<Choice>,
    saved: bool,
}

impl State {
    pub(super) fn last(&self, round: u64) -> Option<&Round> {
        self.last.as_ref().filter(|last| last.round == round)
    }

    pub(super) fn round_pending(&self) -> bool {
        self.round
    }

    pub(super) fn target_pending(&self, key: Key) -> bool {
        self.targets.contains_key(&key)
    }

    pub(super) fn cancel_target(&mut self, key: Key) {
        self.targets.remove(&key);
    }

    pub(super) fn cancel(&mut self) {
        self.round = false;
        self.targets.clear();
    }
}

impl Round {
    pub(super) fn launch(&self) -> Launch<'_> {
        Launch {
            mode: self.mode,
            profile: Some(&self.profile),
            prepared: None,
            goal: &self.goal,
            continuation: None,
        }
    }

    pub(super) fn choice(&self, key: Key) -> Option<&Choice> {
        self.choices.iter().find(|choice| choice.target.key == key)
    }
}

impl Group {
    pub(super) fn member_for(&self, ticket: Ticket) -> Option<window::Id> {
        self.members
            .iter()
            .find_map(|(&member, &candidate)| (candidate == ticket).then_some(member))
    }

    fn can_control_targets(&self) -> bool {
        !self.closing
            && !self.restarts.round_pending()
            && !self.preparing
            && !self.comparison.busy()
            && !self.report_writing
            && !self.saving_conversation
            && self
                .conversation
                .as_ref()
                .is_none_or(|chat| !chat.saving && !chat.loading)
            && self.save_name.is_none()
            && self.workflow.is_none()
    }

    pub(super) fn can_hold_target(&self, ticket: Ticket) -> bool {
        self.can_control_targets()
            && self.mode != SwarmMode::Snippet
            && !self.restarts.target_pending(ticket.key())
            && self.queue.as_ref().is_some_and(|queue| {
                !queue.paused()
                    && queue.current(ticket).is_some_and(|row| {
                        !row.status.finished() && row.status != Status::Cancelling
                    })
            })
    }

    pub(super) fn can_pause_round(&self) -> bool {
        self.can_control_targets() && self.queue.as_ref().is_some_and(|queue| !queue.complete())
    }

    pub(super) fn can_restart_target(&self, ticket: Ticket) -> bool {
        self.can_control_targets()
            && self.mode != SwarmMode::Snippet
            && self.restarts.last(self.round).is_some_and(|last| {
                last.mode == self.mode && self.profile.as_ref() == Some(&last.profile)
            })
            && self.saved_round != Some(self.round)
            && !self.restarts.target_pending(ticket.key())
            && self
                .conversation
                .as_ref()
                .is_none_or(|chat| chat.active.is_some())
            && self.queue.as_ref().is_some_and(|queue| {
                queue.current(ticket).is_some_and(|row| {
                    row.status != Status::Queued
                        && row.status != Status::Cancelling
                        && (row.status.finished() || self.member_for(ticket).is_some())
                })
            })
    }

    pub(super) fn can_restart_round(&self) -> bool {
        self.can_control_targets()
            && !self
                .conversation
                .as_ref()
                .is_some_and(|chat| chat.review.is_some())
            && self.restarts.last(self.round).is_some_and(|last| {
                self.profiles.contains(&last.profile)
                    && self
                        .conversation
                        .as_ref()
                        .is_none_or(|chat| !chat.saving && !chat.loading)
                    && last.saved == self.conversation.is_some()
            })
    }

    pub(super) fn remember_round(&mut self) {
        self.restarts.cancel();
        let active = self
            .conversation
            .as_ref()
            .and_then(|chat| chat.active.as_ref());
        self.restarts.last = self
            .profile
            .as_ref()
            .filter(|_| self.mode != SwarmMode::Snippet)
            .map(|profile| Round {
                round: self.round,
                mode: self.mode,
                profile: profile.clone(),
                goal: active
                    .map_or(self.goal.as_str(), |active| &active.reviewed.question)
                    .to_owned(),
                choices: self
                    .choices
                    .iter()
                    .filter(|choice| self.selected.contains(&choice.target.key))
                    .cloned()
                    .collect(),
                saved: active.is_some(),
            });
    }

    pub(super) fn retry_finished_targets(&mut self) {
        let Some(queue) = &mut self.queue else { return };
        self.restarts.targets.retain(|&key, ticket| {
            let Some(row) = queue.current(*ticket) else {
                return false;
            };
            if !row.status.finished() {
                return true;
            }
            if queue.retry(*ticket) {
                self.comparison.reset_target(key);
                if let Some(active) = self
                    .conversation
                    .as_mut()
                    .and_then(|chat| chat.active.as_mut())
                {
                    active.reset_target(key);
                }
                self.notifications.reset_target(key);
                self.notice = "Target restarted with fresh reviews.".into();
            }
            false
        });
    }
}

impl Workspace {
    pub(super) fn request_swarm_target_restart(&mut self, group: &mut Group, ticket: Ticket) {
        if let Err(error) = self.swarm_policy_for(group.mode) {
            group.notice = error.into();
            return;
        }
        group.batch_review = None;
        group.restarts.targets.insert(ticket.key(), ticket);
        self.cancel_swarm_target(group, ticket.key());
        group.notice = "Waiting for the current target attempt to stop…".into();
    }

    pub(super) fn request_swarm_round_restart(&mut self, group: &mut Group) {
        let Some(last) = group.restarts.last(group.round) else {
            return;
        };
        if let Err(error) = self.swarm_policy_for(last.mode) {
            group.notice = error.into();
            return;
        }
        group.restarts.targets.clear();
        group.restarts.round = true;
        group.batch_review = None;
        group.notifications.stop();
        group.comparison.clear_round();
        if let Some(chat) = &mut group.conversation {
            chat.active = None;
            chat.review = None;
        }
        if let Some(queue) = &mut group.queue {
            queue.set_paused(true);
            let keys: Vec<_> = queue.rows().iter().map(|row| row.target.key).collect();
            for key in keys {
                self.cancel_swarm_target(group, key);
            }
        }
        group.notice = "Waiting for the current Swarm attempts to stop…".into();
    }

    pub(super) fn restart_drained_swarm_round(&mut self, group: &mut Group) {
        if !group.restarts.round_pending()
            || !group.members.is_empty()
            || group.journals_pending != 0
            || group.queue.as_ref().is_some_and(|queue| !queue.complete())
        {
            return;
        }
        group.restarts.round = false;
        let Some(last) = group.restarts.last(group.round) else {
            return;
        };
        if self.exiting || group.closing {
            return;
        }
        if let Err(error) = self.swarm_policy_for(last.mode) {
            group.notice = error.into();
            return;
        }
        let profile = self
            .ai_configuration()
            .and_then(|config| last.profile.transport(&config).ok());
        if profile.is_none() || !group.profiles.contains(&last.profile) {
            group.notice =
                "The previous AI profile is unavailable. Choose a current profile.".into();
            return;
        }
        group.mode = last.mode;
        group.profile = Some(last.profile.clone());
        group.goal.clone_from(&last.goal);
        group.selected = last
            .choices
            .iter()
            .map(|choice| choice.target.key)
            .collect();
        for previous in &last.choices {
            if let Some(choice) = group
                .choices
                .iter_mut()
                .find(|choice| choice.target.key == previous.target.key)
            {
                *choice = previous.clone();
            } else {
                group.choices.push(previous.clone());
            }
        }
        if let Some(chat) = &mut group.conversation {
            chat.profile_selection = last.profile.reference.clone();
            group.notice = match self.review_saved_swarm_follow_up(group) {
                Ok(()) => "Review the current saved targets before restarting the Swarm.".into(),
                Err(error) => error,
            };
            return;
        }
        group.round += 1;
        group.journal_run = None;
        group.prepared = None;
        self.start_reviewed_swarm_queue(group);
    }
}
