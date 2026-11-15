use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use iced::{Subscription, Task};

use crate::ai_manager::{ProfileId, lm_studio_auto};
use crate::clipboard_policy::ClipboardPolicy;
use crate::local_ai::{Cancel, LmStudioAutoModel, WORKERS_BUSY};
use crate::workspace::scheduler_access::desktop_clock;

use super::{Desktop, Message, Workspace};

const PARALLEL: usize = 2;

#[derive(Clone, Debug)]
pub enum Event {
    Poll,
    Resolved(u64, Result<LmStudioAutoModel, String>),
}

struct Lookup {
    selection: lm_studio_auto::Selection,
    started: Instant,
    _cancel: Cancel,
}

#[derive(Default)]
pub(super) struct State {
    active: BTreeMap<u64, Lookup>,
    next: u64,
    forced: Option<Instant>,
}

fn message(event: Event) -> Desktop {
    Desktop::AiManager(Message::LmStudioAuto(event))
}

impl Workspace {
    pub(in crate::workspace) fn ai_model_resolution_pending(&self, id: ProfileId) -> bool {
        !self.exiting
            && !self.scheduler_draining
            && self.ai_features_enabled()
            && self.ai_manager.config.ai_model_resolution_pending(id)
    }

    pub(super) fn lm_studio_auto_subscription(&self) -> Subscription<Desktop> {
        if !self.exiting
            && !self.scheduler_draining
            && self.ai_features_enabled()
            && self
                .ai_manager
                .config
                .lm_studio_auto_selections()
                .next()
                .is_some()
        {
            desktop_clock(lm_studio_auto::POLL).map(|()| message(Event::Poll))
        } else {
            Subscription::none()
        }
    }

    pub(super) fn update_lm_studio_auto(&mut self, event: Event) -> Task<Desktop> {
        if let Event::Resolved(ticket, result) = event {
            let Some(lookup) = self.ai_manager.lm_studio_auto.active.remove(&ticket) else {
                return Task::none();
            };
            let policy = ClipboardPolicy::load();
            if !self.exiting
                && !self.scheduler_draining
                && self.ai_features_enabled()
                && policy.problem.is_none()
                && policy.ai_allowed
                && self
                    .ai_manager
                    .config
                    .lm_studio_auto_selections()
                    .any(|selection| selection == lookup.selection)
            {
                lm_studio_auto::publish(lookup.selection, lookup.started, result);
            }
        }
        self.poll_lm_studio_auto(false)
    }

    pub(in crate::workspace) fn poll_lm_studio_auto(&mut self, force: bool) -> Task<Desktop> {
        if self.exiting || self.scheduler_draining || !self.ai_features_enabled() {
            self.ai_manager.lm_studio_auto.active.clear();
            self.ai_manager.lm_studio_auto.forced = None;
            return Task::none();
        }
        let policy = ClipboardPolicy::load();
        let state = &mut self.ai_manager.lm_studio_auto;
        if policy.problem.is_some() || !policy.ai_allowed {
            state.active.clear();
            state.forced = None;
            return self.apply_lm_studio_auto_projection(&policy);
        }
        if force {
            state.forced = Some(Instant::now());
        }
        let selections: Vec<_> = self.ai_manager.config.lm_studio_auto_selections().collect();
        state
            .active
            .retain(|_, lookup| selections.contains(&lookup.selection));
        let mut candidates: Vec<_> = selections
            .into_iter()
            .filter_map(|selection| {
                if state
                    .active
                    .values()
                    .any(|lookup| lookup.selection == selection)
                {
                    return None;
                }
                lm_studio_auto::refresh_order(&selection, state.forced)
                    .map(|order| (order, selection))
            })
            .collect();
        candidates.sort_by_key(|(order, _)| *order);
        if candidates.is_empty() && state.active.is_empty() {
            state.forced = None;
        }
        let mut tasks = Vec::new();
        for (_, selection) in candidates
            .into_iter()
            .take(PARALLEL.saturating_sub(state.active.len()))
        {
            let started = Instant::now();
            match selection.resolve() {
                Ok(pending) => {
                    state.next = state.next.wrapping_add(1);
                    let ticket = state.next;
                    state.active.insert(
                        ticket,
                        Lookup {
                            selection,
                            started,
                            _cancel: pending.cancel,
                        },
                    );
                    tasks.push(Task::perform(
                        async move {
                            pending.reply.recv().await.unwrap_or_else(|_| {
                                Err("The model metadata worker stopped.".into())
                            })
                        },
                        move |result| message(Event::Resolved(ticket, result)),
                    ));
                }
                Err(error) if error == WORKERS_BUSY => break,
                Err(error) => {
                    lm_studio_auto::publish(selection, started, Err(error));
                }
            }
        }
        tasks.push(self.apply_lm_studio_auto_projection(&policy));
        Task::batch(tasks)
    }

    fn apply_lm_studio_auto_projection(&mut self, policy: &ClipboardPolicy) -> Task<Desktop> {
        let mut projected = self.ai_manager.config.as_ref().clone();
        if !projected.refresh_lm_studio_auto(policy) {
            return Task::none();
        }
        let previous_vision = self.configured_automatic_journal_vision();
        let projected = Arc::new(projected);
        if let Some((_, dialog)) = &mut self.ai_manager.surface {
            dialog.install_lm_studio_auto_projection(&projected);
        }
        let previous = std::mem::replace(&mut self.ai_manager.config, Arc::clone(&projected));
        self.preserve_journal_summary_for_policy_projection(&previous);
        self.preserve_terminal_ai_reviews_for_policy_projection(&previous);
        self.preserve_snippet_analysis_reviews_for_policy_projection(&previous);
        self.apply_ai_manager_projection(previous_vision);
        self.refresh_terminal_ai_profile_choices(&projected);
        self.resume_policy_auto_consumers()
    }
}
