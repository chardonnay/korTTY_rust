//! Publish masked Swarm conversations only from committed scheduler results.

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::{Task, window};
use kortty_jobs::{ProfileRef, RunId, SwarmChatOrigin, SwarmReport};

use crate::clipboard_policy::ClipboardPolicy;
use crate::workspace::saved_chats::swarm;

use super::{Message, Workspace, profile};

#[cfg(test)]
mod tests;

const RETRY: Duration = Duration::from_secs(10);

#[derive(Default)]
pub(in crate::workspace) struct Publisher {
    publishing: Option<u64>,
    opening: Option<u64>,
    retry_at: BTreeMap<String, Instant>,
}

type SeedResult = Result<Option<Arc<swarm::Record>>, String>;

#[derive(Clone)]
pub struct Event {
    token: u64,
    root: PathBuf,
    nonce: String,
    opening: Option<(window::Id, RunId)>,
    result: SeedResult,
}

impl fmt::Debug for Event {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Saved Swarm conversation event")
    }
}

impl Workspace {
    pub(in crate::workspace) fn swarm_chat_access_allowed(&self) -> bool {
        self.ai_chat_enabled() && self.clipboard.policy.ai_swarm_allowed()
    }

    fn swarm_chat_profile(&self) -> Option<PathBuf> {
        if self.exiting || self.scheduler_draining || !self.swarm_chat_access_allowed() {
            return None;
        }
        let root = profile::root().ok()?;
        (self.scheduler.as_ref()?.profile_root() == root.canonicalize().ok()?).then_some(root)
    }

    pub(super) fn swarm_chat_seed_needs_tick(&self) -> bool {
        self.scheduler_swarm_chat.publishing.is_none()
            && self.scheduler_swarm_chat.opening.is_none()
            && !self.exiting
            && !self.scheduler_draining
            && self.swarm_chat_access_allowed()
            && self
                .scheduler
                .as_ref()
                .is_some_and(|scheduler| scheduler.has_swarm_chat_seed())
    }

    pub(super) fn tick_swarm_chat_seed(&mut self) -> Task<Message> {
        if !self.swarm_chat_seed_needs_tick() {
            return Task::none();
        }
        let now = Instant::now();
        self.scheduler_swarm_chat
            .retry_at
            .retain(|_, retry| now < *retry);
        let Some((root, seed)) = self
            .swarm_chat_profile()
            .zip(self.scheduler.as_ref().and_then(|scheduler| {
                scheduler.next_swarm_chat_seed(|seed| {
                    !self
                        .scheduler_swarm_chat
                        .retry_at
                        .contains_key(&seed.origin.nonce)
                })
            }))
        else {
            return Task::none();
        };
        let token = self.allocate();
        self.scheduler_swarm_chat.publishing = Some(token);
        seed_task(token, root, seed.origin, seed.report, None)
    }

    pub(super) fn open_scheduler_swarm_chat(
        &mut self,
        view_id: window::Id,
        run: RunId,
    ) -> Task<Message> {
        if self.scheduler_swarm_chat.opening.is_some() {
            return Task::none();
        }
        if self.scheduler_swarm_chat.publishing.is_some() {
            if let Some(view) = self.scheduler_views.get_mut(&view_id) {
                view.report_status(
                    "A Swarm conversation is being saved. Open it again when the save finishes."
                        .into(),
                );
            }
            return Task::none();
        }
        let Some(root) = self.swarm_chat_profile() else {
            if let Some(view) = self.scheduler_views.get_mut(&view_id) {
                view.report_status(
                    "Opening Swarm conversations is unavailable under the current settings.".into(),
                );
            }
            return Task::none();
        };
        let source = self
            .scheduler
            .as_ref()
            .zip(self.scheduler_views.get(&view_id))
            .and_then(|(scheduler, view)| scheduler.selected_swarm_chat(view, run))
            .map(|(report, origin)| (report.clone(), origin.clone()));
        let Some((report, origin)) = source else {
            return Task::none();
        };
        let token = self.allocate();
        self.scheduler_swarm_chat.opening = Some(token);
        seed_task(token, root, origin, report, Some((view_id, run)))
    }

    pub(in crate::workspace) fn finish_scheduler_swarm_chat(
        &mut self,
        event: Event,
    ) -> Task<Message> {
        let active = if event.opening.is_some() {
            &mut self.scheduler_swarm_chat.opening
        } else {
            &mut self.scheduler_swarm_chat.publishing
        };
        if active.take_if(|token| *token == event.token).is_none() {
            return Task::none();
        }
        if self.swarm_chat_profile().as_ref() != Some(&event.root) {
            return Task::none();
        }
        if let Ok(Some(record)) = &event.result {
            self.record_saved_swarm(&event.root, record);
        }
        let publication = match &event.result {
            Ok(Some(record)) if record.sync_failed => {
                Err("The conversation directory could not be synchronized".into())
            }
            Ok(_) => self
                .scheduler
                .as_mut()
                .expect("current scheduler profile")
                .acknowledge_swarm_chat_seed(&event.nonce)
                .map(|_| ())
                .map_err(|error| error.to_string()),
            Err(error) => Err(error.clone()),
        };
        if let Err(error) = publication {
            if event.opening.is_none() {
                self.scheduler_swarm_chat
                    .retry_at
                    .insert(event.nonce.clone(), Instant::now() + RETRY);
                for view in self.scheduler_views.values_mut() {
                    view.report_status(format!(
                        "Could not finish saving the Swarm conversation ({error})."
                    ));
                }
            }
        } else {
            self.scheduler_swarm_chat.retry_at.remove(&event.nonce);
        }
        let Some((view_id, run)) = event.opening else {
            return Task::none();
        };
        let current = self
            .scheduler
            .as_ref()
            .zip(self.scheduler_views.get(&view_id))
            .and_then(|(scheduler, view)| scheduler.selected_swarm_chat(view, run))
            .is_some_and(|(_, origin)| origin.nonce == event.nonce);
        if !current {
            return Task::none();
        }
        match event.result {
            Ok(Some(record)) => self
                .scheduler_owners
                .get(&view_id)
                .copied()
                .or_else(|| self.hosted_tool_owner(view_id))
                .map_or_else(Task::none, |owner| {
                    self.open_saved_swarm(owner, event.root, record.as_ref().clone())
                }),
            result => {
                let notice = match result {
                    Ok(None) => "This saved Swarm conversation was deleted.".into(),
                    Err(error) => format!("Could not open the Swarm conversation ({error})."),
                    Ok(Some(_)) => unreachable!("saved conversation already handled"),
                };
                if let Some(view) = self.scheduler_views.get_mut(&view_id) {
                    view.report_status(notice);
                }
                Task::none()
            }
        }
    }
}

fn seed_task(
    token: u64,
    root: PathBuf,
    source: SwarmChatOrigin,
    report: SwarmReport,
    opening: Option<(window::Id, RunId)>,
) -> Task<Message> {
    Task::future(async move {
        let result = seed(&root, &source, report)
            .map(|record| record.map(Arc::new))
            .map_err(|error| error.to_string());
        Message::SchedulerSwarmChat(Event {
            token,
            root,
            nonce: source.nonce,
            opening,
            result,
        })
    })
}

fn seed(
    root: &Path,
    source: &SwarmChatOrigin,
    report: SwarmReport,
) -> io::Result<Option<swarm::Record>> {
    let origin = swarm::Origin::from_id(&source.nonce)?;
    let label = match &source.profile {
        ProfileRef::Numeric(id) => format!("AI profile {id}"),
        ProfileRef::Policy(_) => "Organization AI profile".into(),
    };
    swarm::seed(
        root,
        &origin,
        source.profile.clone(),
        &label,
        report,
        || {
            let policy = ClipboardPolicy::load();
            if profile::root()? == root && policy.ai_chat_allowed && policy.ai_swarm_allowed() {
                Ok(())
            } else {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "The saved Swarm conversation policy or profile changed",
                ))
            }
        },
    )
}
