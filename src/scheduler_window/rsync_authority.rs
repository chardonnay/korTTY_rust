//! Revoke reviewed rsync publication when the saved job changes during a run.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use kortty_jobs::{JobDefinition, JobGraph, JobId, RunId, RunRequest, RunTrigger};

use super::rsync::standing::Lease;
use super::{ConnectionRef, ConnectionStore, SchedulerWindow, SshCommand};

const REVOKED: &str = "The saved rsync receiver approval is no longer active.";

#[derive(Clone)]
pub(crate) struct RunContext {
    run: RunId,
    job: JobId,
    routes: Arc<[ConnectionRef]>,
    connection_store: PathBuf,
    allowed: Arc<AtomicBool>,
    standing: Option<Arc<Lease>>,
}

impl RunContext {
    pub(crate) fn standing(&self) -> Option<&Lease> {
        self.standing.as_deref()
    }

    pub(crate) fn require_current(
        &self,
        run: RunId,
        job: &JobId,
        reference: &ConnectionRef,
    ) -> Result<(), &'static str> {
        if self.run != run
            || &self.job != job
            || !self.routes.contains(reference)
            || !self.allowed.load(Ordering::Acquire)
        {
            return Err(REVOKED);
        }
        let store = ConnectionStore::open(self.connection_store.clone())
            .map_err(|_| "The saved rsync connections are unavailable.")?;
        let shared = if reference.shared_identity().is_some() {
            super::SharedTargets::load_reviewed(&store, [reference.clone()])?
        } else {
            super::SharedTargets::default()
        };
        shared
            .require_saved(&store, reference)
            .map_err(|_| "The saved rsync route changed. Review the transfer again.")?;
        if let Some(standing) = &self.standing {
            standing.require_current().map_err(|_| REVOKED)?;
        }
        self.allowed
            .load(Ordering::Acquire)
            .then_some(())
            .ok_or(REVOKED)
    }
}

#[derive(Default)]
pub(super) struct Runs(BTreeMap<RunId, Registered>);

struct Registered {
    context: RunContext,
    approved_action: Arc<SshCommand>,
    enabled: bool,
    definition: Option<JobDefinition>,
}

impl Drop for Registered {
    fn drop(&mut self) {
        self.context.allowed.store(false, Ordering::Release);
    }
}

impl Runs {
    pub(super) fn retain_current(&mut self, graph: &JobGraph<SshCommand>) {
        for entry in self.0.values() {
            if !graph.job(&entry.context.job).is_some_and(|job| {
                job.enabled() == entry.enabled
                    && job.action() == entry.approved_action.as_ref()
                    && entry.definition.as_ref().is_none_or(|approved| {
                        job.schedule() == approved.schedule.as_ref()
                            && job.dependencies() == approved.dependencies
                            && job.mode() == approved.mode
                            && job.max_age_minutes() == approved.max_age_minutes
                    })
            }) {
                entry.context.allowed.store(false, Ordering::Release);
            }
        }
    }

    pub(super) fn finish(&mut self, run: RunId) -> bool {
        self.0.remove(&run).is_some()
    }
}

pub(crate) fn uses_reviewed_receiver(action: &SshCommand) -> bool {
    action
        .remote
        .as_ref()
        .and_then(super::RemoteAction::rsync_transfer)
        .is_some_and(|(transfer, upload)| transfer.requires_reviewed_receiver(upload))
}

impl SchedulerWindow {
    #[cfg(test)]
    pub(crate) fn rsync_run_context(
        &mut self,
        request: &RunRequest<SshCommand>,
    ) -> Option<RunContext> {
        self.try_rsync_run_context(request).ok().flatten()
    }

    pub(crate) fn try_rsync_run_context(
        &mut self,
        request: &RunRequest<SshCommand>,
    ) -> Result<Option<RunContext>, String> {
        let Some((transfer, upload)) = request
            .action
            .remote
            .as_ref()
            .and_then(super::RemoteAction::rsync_transfer)
            .filter(|(transfer, upload)| transfer.requires_reviewed_receiver(*upload))
        else {
            return Ok(None);
        };
        if request.cancellation.is_requested() {
            return Ok(None);
        }
        let current = self.scheduler.graph().job(&request.job).ok_or(REVOKED)?;
        if current.action() != request.action.as_ref() {
            return Err(REVOKED.into());
        }
        let connection_store = self
            .connection_store_path
            .clone()
            .or_else(|| crate::profile::connections_path().ok())
            .ok_or("The saved rsync connections are unavailable.")?;
        let automatic = matches!(
            request.trigger,
            RunTrigger::Scheduled | RunTrigger::Dependency
        );
        if transfer.requires_automatic_receiver(upload) && !automatic {
            return Err(
                "This reviewed rsync transfer requires an approved Calendar or Chain firing."
                    .into(),
            );
        }
        let configured = transfer.standing.is_some();
        let standing = if automatic && configured {
            let occurrence = self
                .scheduler
                .active_firing_occurrence(request)
                .cloned()
                .ok_or("The automatic rsync firing is no longer current.")?;
            if !current.enabled() {
                return Err(REVOKED.into());
            }
            Some(Arc::new(
                self.rsync_standing
                    .claim(
                        current,
                        &self.zone,
                        occurrence,
                        std::time::SystemTime::now(),
                    )
                    .map_err(|error| error.to_string())?,
            ))
        } else {
            None
        };
        let context = RunContext {
            run: request.id,
            job: request.job.clone(),
            routes: request.action.targets().cloned().collect(),
            connection_store,
            allowed: Arc::new(AtomicBool::new(true)),
            standing,
        };
        self.rsync_runs.0.insert(
            request.id,
            Registered {
                context: context.clone(),
                approved_action: Arc::clone(&request.action),
                enabled: current.enabled(),
                definition: (automatic && configured).then(|| current.definition()),
            },
        );
        Ok(Some(context))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job_executor::Completion;
    use crate::scheduler_window::Message;
    use kortty_jobs::{Job, RunOutcome, SchedulerStore};
    use serde_json::json;
    use std::time::{Duration, SystemTime};

    #[test]
    fn revoked_rsync_run_keeps_all_absolute_recovery_paths_in_reopened_history() {
        let profile = crate::snippets::tests::Profile::new();
        let path = profile.0.join("jobs.json");
        let id = JobId::new("receiver-history").unwrap();
        let reference = json!({
            "id": 1,
            "target": { "host": "rsync.example", "port": 22, "username": "fixture" }
        });
        let source = profile.0.join("source");
        let program = profile.0.join("rsync");
        let destination = "/srv/receiver";
        let settings = json!({ "include_source_name": false, "delete": true });
        let inventory = json!({ "entries": 0, "files": 0, "bytes": 0, "digest": vec![1_u8; 32] });
        let action: SshCommand = serde_json::from_value(json!({
            "connection": reference,
            "remote": {
                "kind": "rsync_upload",
                "directory": true,
                "program": program,
                "local": source,
                "remote": destination,
                "existing": settings,
                "approval": {
                    "program": program,
                    "digest": vec![1_u8; 32],
                    "reference": reference,
                    "pins": ["rsync.example SHA256:fixture"],
                    "directory_upload": { "source": source, "destination": destination, "inventory": inventory },
                    "existing": { "destination": destination, "settings": settings,
                        "snapshot": { "inventory": inventory, "fingerprint": vec![2_u8; 32] },
                    },
                },
            },
        })).unwrap();
        let graph = JobGraph::new(vec![
            Job::new(id.clone(), "Existing receiver", action).unwrap(),
        ])
        .unwrap();
        SchedulerStore::open(&path)
            .unwrap()
            .commit_with_actions(&graph, &[], &[])
            .unwrap();
        let mut scheduler = SchedulerWindow::open(path.clone()).unwrap();
        scheduler.connection_store_path = Some(profile.0.join("connections.json"));
        scheduler.set_execution_allowed(true);
        let now = SystemTime::now();
        let started = scheduler.scheduler.start_manual(&id, now).unwrap();
        let request = scheduler.receive(started, None, now).pop().unwrap();
        let context = scheduler.rsync_run_context(&request).unwrap();

        let empty = JobGraph::<SshCommand>::new(Vec::new()).unwrap();
        scheduler.rsync_runs.retain_current(&empty);
        let route = request.action.connection.as_ref().unwrap();
        assert!(context.require_current(request.id, &id, route).is_err());
        let parent = format!("/{}", vec!["a".repeat(254); 16].join("/"));
        let recoveries: Vec<_> = (0..8)
            .map(|target| {
                format!("{parent}/target-{target}/.kortty-{target:032x}.kortty-part/original")
            })
            .collect();
        let detail = recoveries.join(" · ");
        assert!(detail.len() > 4096);
        assert_eq!(
            super::super::safe_detail(&detail, false).unwrap().len(),
            1024
        );
        scheduler.update_runtime_at(
            Message::Finished(Completion {
                id: request.id,
                outcome: RunOutcome::Failed { exit_code: None },
                detail: Some(detail.clone()),
                output: None,
                recording: None,
                swarm_report: None,
            }),
            now + Duration::from_secs(1),
        );
        drop(scheduler);

        let mut store = SchedulerStore::open(&path).unwrap();
        store.commit_with_actions(&empty, &[], &[]).unwrap();
        drop(store);
        let reopened = SchedulerStore::open(path).unwrap();
        assert_eq!(reopened.journal_detail(request.id), Some(detail.as_str()));
    }
}
