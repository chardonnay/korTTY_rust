use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use iced::{Subscription, Task};

use crate::ai_manager::{ProfileId, policy_auto};
use crate::clipboard_policy::{ClipboardPolicy, PolicyAiProfile};
use crate::local_ai::{self, Cancel, PolicyAutoModel};
use crate::workspace::journal_access::summaries::Worker;
use crate::workspace::scheduler_access::desktop_clock;

use super::{Desktop, Message, Workspace};

const PARALLEL: usize = 2;

#[derive(Clone, Debug)]
pub enum Event {
    Poll,
    Resolved(u64, Result<PolicyAutoModel, String>),
}

struct Lookup {
    definition: Arc<PolicyAiProfile>,
    started: Instant,
    _cancel: Cancel,
}

#[derive(Default)]
pub(super) struct State {
    active: BTreeMap<u64, Lookup>,
    next: u64,
    forced: Option<Instant>,
    suspended: bool,
}

fn message(event: Event) -> Desktop {
    Desktop::AiManager(Message::PolicyAuto(event))
}

impl Workspace {
    pub(in crate::workspace) fn policy_auto_model_pending(&self, id: ProfileId) -> bool {
        !self.exiting
            && !self.scheduler_draining
            && self.ai_features_enabled()
            && !self.ai_manager.policy_auto.suspended
            && self.ai_manager.config.policy_auto_model_pending(id)
    }

    pub(super) fn policy_auto_subscription(&self) -> Subscription<Desktop> {
        if !self.exiting
            && !self.scheduler_draining
            && self.ai_features_enabled()
            && self
                .ai_manager
                .config
                .policy_auto_definitions()
                .next()
                .is_some()
        {
            desktop_clock(policy_auto::POLL).map(|()| message(Event::Poll))
        } else {
            Subscription::none()
        }
    }

    pub(super) fn update_policy_auto(&mut self, event: Event) -> Task<Desktop> {
        let Event::Resolved(ticket, result) = event else {
            return self.poll_policy_auto_profiles(false);
        };
        let Some(lookup) = self.ai_manager.policy_auto.active.remove(&ticket) else {
            return Task::none();
        };
        if self.exiting || self.scheduler_draining || !self.ai_features_enabled() {
            return Task::none();
        }
        let installed = ClipboardPolicy::load();
        policy_auto::publish(lookup.definition, lookup.started, result, &installed);
        let changed = self.refresh_policy_auto_projection(&installed);
        let next = self.poll_policy_auto_profiles(false);
        if changed {
            Task::batch([next, self.resume_policy_auto_consumers()])
        } else {
            next
        }
    }

    pub(super) fn resume_policy_auto_consumers(&mut self) -> Task<Desktop> {
        Task::batch([
            self.enqueue_jobs(Vec::new()),
            self.update_journal_summary_worker(Worker::Pulse),
        ])
    }

    pub(in crate::workspace) fn poll_policy_auto_profiles(&mut self, force: bool) -> Task<Desktop> {
        if self.exiting || self.scheduler_draining || !self.ai_features_enabled() {
            self.ai_manager.policy_auto.active.clear();
            self.ai_manager.policy_auto.forced = None;
            return Task::none();
        }
        let installed = ClipboardPolicy::load();
        let changed = self.refresh_policy_auto_projection(&installed);
        let state = &mut self.ai_manager.policy_auto;
        state
            .active
            .retain(|_, lookup| policy_auto::current(&installed, &lookup.definition));
        let suspended = installed.problem.is_some() || !installed.ai_allowed;
        let newly_suspended = suspended && !state.suspended;
        state.suspended = suspended;
        if suspended {
            state.forced = None;
            return if newly_suspended || changed {
                self.resume_policy_auto_consumers()
            } else {
                Task::none()
            };
        }
        if force {
            state.forced = Some(Instant::now());
        }
        let mut candidates: Vec<_> = self
            .ai_manager
            .config
            .policy_auto_definitions()
            .filter(|definition| {
                !state
                    .active
                    .values()
                    .any(|lookup| lookup.definition.as_ref() == definition.as_ref())
            })
            .filter_map(|definition| {
                policy_auto::refresh_order(definition, state.forced)
                    .map(|order| (order, Arc::clone(definition)))
            })
            .collect();
        candidates.sort_by_key(|(order, _)| *order);
        if candidates.is_empty() && state.active.is_empty() {
            state.forced = None;
        }
        let mut tasks = Vec::new();
        let mut failed = false;
        for (_, definition) in candidates
            .into_iter()
            .take(PARALLEL.saturating_sub(state.active.len()))
        {
            let started = Instant::now();
            let pending = match local_ai::resolve_policy_auto_model(&definition) {
                Ok(pending) => pending,
                Err(error) => {
                    failed |= policy_auto::publish(definition, started, Err(error), &installed);
                    continue;
                }
            };
            state.next = state.next.wrapping_add(1);
            let ticket = state.next;
            state.active.insert(
                ticket,
                Lookup {
                    definition,
                    started,
                    _cancel: pending.cancel,
                },
            );
            tasks.push(Task::perform(
                async move {
                    pending
                        .reply
                        .recv()
                        .await
                        .unwrap_or_else(|_| Err("The model metadata worker stopped.".into()))
                },
                move |result| message(Event::Resolved(ticket, result)),
            ));
        }
        if failed {
            self.refresh_policy_auto_projection(&installed);
        }
        if failed || changed {
            tasks.push(self.resume_policy_auto_consumers());
        }
        Task::batch(tasks)
    }

    fn refresh_policy_auto_projection(&mut self, installed: &ClipboardPolicy) -> bool {
        let Ok(projected) = self
            .ai_manager
            .config
            .as_ref()
            .clone()
            .with_policy_profiles(installed)
        else {
            return false;
        };
        if !projected.policy_profiles_changed(&self.ai_manager.config) {
            return false;
        }
        let previous_vision = self.configured_automatic_journal_vision();
        let projected = Arc::new(projected);
        if let Some((_, dialog)) = &mut self.ai_manager.surface {
            dialog.install_policy_profile_projection(&projected);
        }
        let previous = std::mem::replace(&mut self.ai_manager.config, Arc::clone(&projected));
        self.preserve_journal_summary_for_policy_projection(&previous);
        self.preserve_terminal_ai_reviews_for_policy_projection(&previous);
        self.preserve_snippet_analysis_reviews_for_policy_projection(&previous);
        self.apply_ai_manager_projection(previous_vision);
        self.refresh_terminal_ai_profile_choices(&projected);
        true
    }
}
