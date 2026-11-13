//! Workspace ownership and cancellation of background knowledge reconciliation.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use iced::{Subscription, Task};

use crate::ai_manager::{Configuration, knowledge_sync};
use crate::workspace::{Message as Desktop, Workspace, scheduler_access::desktop_clock};

const POLL_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, Debug)]
pub(crate) enum Event {
    Tick,
    Finished(u64, knowledge_sync::Report),
}

#[derive(Default)]
pub(super) struct State {
    next: usize,
    ticket: u64,
    running: Option<Running>,
    pub(super) statuses: BTreeMap<String, String>,
}

struct Running {
    baseline: Arc<Configuration>,
    store: String,
    cancel: Option<knowledge_sync::Cancel>,
}

impl Workspace {
    pub(super) fn knowledge_sync_subscription(&self) -> Subscription<Desktop> {
        if self.automatic_knowledge_allowed() && knowledge_sync::available(&self.ai_manager.config)
        {
            desktop_clock(POLL_INTERVAL).map(|()| message(Event::Tick))
        } else {
            Subscription::none()
        }
    }

    fn automatic_knowledge_allowed(&self) -> bool {
        !self.exiting
            && !self.scheduler_draining
            && self.ai_features_enabled()
            && self.ai_manager.skill_rule_write.is_none()
            && self
                .ai_manager
                .surface
                .as_ref()
                .is_none_or(|(_, dialog)| dialog.automatic_knowledge_idle())
    }

    pub(in crate::workspace) fn reconcile_automatic_knowledge(&mut self) {
        let allowed = self.automatic_knowledge_allowed();
        if let Some(running) = &mut self.ai_manager.knowledge_sync.running
            && (!allowed || self.ai_manager.config != running.baseline)
        {
            // The slot remains occupied until this canceled worker has actually returned.
            running.cancel = None;
        }
    }

    pub(super) fn update_knowledge_sync(&mut self, event: Event) -> Task<Desktop> {
        match event {
            Event::Tick => self.poll_automatic_knowledge(),
            Event::Finished(ticket, report) => {
                self.finish_automatic_knowledge(ticket, report);
                Task::none()
            }
        }
    }

    fn poll_automatic_knowledge(&mut self) -> Task<Desktop> {
        if !self.automatic_knowledge_allowed() || self.ai_manager.knowledge_sync.running.is_some() {
            return Task::none();
        }
        let sync = &mut self.ai_manager.knowledge_sync;
        let baseline = Arc::clone(&self.ai_manager.config);
        let Some(attempt) =
            knowledge_sync::start(Arc::clone(&baseline), sync.next, self.vault.as_ref())
        else {
            return Task::none();
        };
        sync.next = attempt.next;
        let store = attempt.store;
        let pending = match attempt.pending {
            Ok(pending) => pending,
            Err(error) => {
                self.knowledge_sync_status(store, knowledge_sync::Report::failed(error).status);
                return Task::none();
            }
        };
        sync.ticket = sync.ticket.wrapping_add(1);
        let ticket = sync.ticket;
        sync.running = Some(Running {
            baseline,
            store,
            cancel: Some(pending.cancel),
        });
        Task::perform(
            async move {
                pending
                    .reply
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err("The automatic knowledge worker stopped.".into()))
                    .unwrap_or_else(knowledge_sync::Report::failed)
            },
            move |report| message(Event::Finished(ticket, report)),
        )
    }

    fn finish_automatic_knowledge(&mut self, ticket: u64, report: knowledge_sync::Report) {
        let sync = &mut self.ai_manager.knowledge_sync;
        if ticket != sync.ticket {
            return;
        }
        let Some(running) = sync.running.take() else {
            return;
        };
        if self.ai_manager.config != running.baseline {
            return;
        }
        if let Some(saved) = report.saved {
            if let Some((_, dialog)) = &mut self.ai_manager.surface {
                dialog.merge_automatic_knowledge(&running.baseline, &saved);
            }
            self.ai_manager.config = saved;
            self.invalidate_snippet_apply_requests();
        }
        if running.cancel.is_some() || report.changed {
            self.knowledge_sync_status(running.store, report.status);
        }
        if report.changed {
            self.invalidate_snippet_knowledge();
            let config = self.ai_chat_configuration();
            self.invalidate_terminal_ai_reviews(config.as_ref());
        }
    }

    fn knowledge_sync_status(&mut self, store: String, status: String) {
        let statuses = &mut self.ai_manager.knowledge_sync.statuses;
        if statuses.len() >= 32 && !statuses.contains_key(&store) {
            statuses.clear();
        }
        statuses.insert(store, status);
        if let Some((_, dialog)) = &mut self.ai_manager.surface {
            dialog.set_automatic_knowledge_status(statuses.clone());
        }
    }
}

fn message(event: Event) -> Desktop {
    Desktop::AiManager(super::Message::KnowledgeSync(event))
}
