use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use iced::{Task, window};

use crate::job_executor::rsync::PrivilegedPreviewMode;
use crate::scheduler_window::rsync::UploadReceiverPreview;
use crate::scheduler_window::{ConnectionRef, SharedTargets, TargetGroups};
use kortty_server_archive::rsync_tree::RecurringReceiver;

use super::{ClipboardPolicy, ConnectionStore, Message, Workspace, job_executor, profile};

impl Workspace {
    pub(super) fn review_scheduler_rsync(&mut self, id: window::Id) -> Task<Message> {
        if let Err(error) = self.current_scheduler_draft_groups(id) {
            return self.report_scheduler_group_error(id, error);
        }
        let Some(view) = self.scheduler_views.get(&id) else {
            return Task::none();
        };
        let Some(request) = view.rsync_review_requests() else {
            return self.report_scheduler_group_error(
                id,
                "Choose saved SSH targets, valid absolute paths and a destination for each route."
                    .into(),
            );
        };
        let targets = request.existing_targets();
        let mixed_targets = request.mixed_targets();
        let privileged_targets = request.privileged_targets();
        let mut credentials = VecDeque::new();
        let mut mixed_credentials = VecDeque::new();
        let mut privileged_credentials = VecDeque::new();
        let reviewed = {
            let result = (|| -> Result<ReviewedRoutes, String> {
                let installed = ClipboardPolicy::load();
                if !self.scheduler_sync_allowed()
                    || installed.problem.is_some()
                    || !installed.file_transfer_allowed
                    || !installed.minimum_isolation.is_none()
                {
                    return Err("Managed policy forbids rsync destination review.".into());
                }
                let privileged = !privileged_targets.is_empty()
                    || mixed_targets.iter().any(|target| {
                        target.elevation == crate::scheduler_window::rsync::Elevation::Passwordless
                    });
                if privileged
                    && (!installed.scheduler_sudo_allowed
                        || !self.clipboard.policy.scheduler_sudo_allowed)
                {
                    return Err("Managed policy forbids privileged rsync review.".into());
                }
                let reviewed = ReviewedRoutes {
                    path: profile::connections_path().map_err(|error| error.to_string())?,
                    groups: view.rsync_review_groups(),
                    references: request.references().cloned().collect(),
                };
                let store = reviewed.open().map_err(|error| error.to_string())?;
                for reference in &reviewed.references {
                    if installed
                        .server_access()
                        .first_blocked(reference.target.servers(reference.jump.as_ref()))
                        .is_some()
                        || self
                            .clipboard
                            .policy
                            .server_access()
                            .first_blocked(reference.target.servers(reference.jump.as_ref()))
                            .is_some()
                    {
                        return Err("Managed policy blocks a selected rsync route.".into());
                    }
                }
                let teamwork = if reviewed.has_teamwork() {
                    Some(self.scheduler_shared_targets(&store)?)
                } else {
                    None
                };
                let authenticate = |reference: &ConnectionRef| {
                    if let Some(identity) = reference.shared_identity() {
                        let (catalog, shared) = teamwork
                            .as_ref()
                            .ok_or("The Teamwork catalogue is unavailable.")?;
                        shared.require(reference)?;
                        let entry = catalog
                            .get(identity)
                            .ok_or("The Teamwork rsync route is unavailable.")?;
                        return self
                            .scheduler_shared_credentials(reference, entry, &store, false, false);
                    }
                    let connection = store
                        .get(reference.id)
                        .ok_or("The saved rsync route is unavailable.")?;
                    job_executor::Credentials::resolve_directory_browser(
                        reference,
                        connection,
                        installed.server_access(),
                        |id, kind, endpoint| self.vault_credential(id, kind, endpoint),
                    )
                };
                for (reference, destination) in targets {
                    let auth = authenticate(&reference)?;
                    credentials.push_back((reference, destination, auth));
                }
                for target in &mixed_targets {
                    let reference = &target.reference;
                    mixed_credentials.push_back((reference.clone(), authenticate(reference)?));
                }
                for (reference, spec) in privileged_targets {
                    let auth = authenticate(&reference)?;
                    privileged_credentials.push_back((reference, spec, auth));
                }
                Ok(reviewed)
            })();
            match result {
                Ok(reviewed) => Arc::new(reviewed),
                Err(error) => return self.report_scheduler_group_error(id, error),
            }
        };
        let Some(view) = self.scheduler_views.get_mut(&id) else {
            return Task::none();
        };
        let completed_review = Arc::clone(&reviewed);
        let mixed_review = reviewed.clone();
        let privileged_review = reviewed.clone();
        view.begin_rsync_review(
            request,
            move |reference, target, trust, cancelled| {
                let (expected, path, auth) = credentials
                    .pop_front()
                    .ok_or("The reviewed rsync credentials are unavailable.")?;
                if &expected != reference || &path != target.destination() {
                    return Err("The rsync route or receiver changed before review.".into());
                }
                let reviewed = Arc::clone(&reviewed);
                let current = move || reviewed.open().map(|_| ());
                match target {
                    UploadReceiverPreview::ExistingDirectory(destination) => {
                        job_executor::rsync::preview_existing(
                            auth,
                            reference,
                            destination,
                            trust,
                            Arc::clone(cancelled),
                            current,
                        )
                        .map(|snapshot| RecurringReceiver::Existing { snapshot })
                    }
                    UploadReceiverPreview::Recurring(target) => {
                        job_executor::rsync::preview_recurring_receiver(
                            auth,
                            reference,
                            target,
                            trust,
                            Arc::clone(cancelled),
                            current,
                        )
                    }
                }
            },
            move |target, trust, cancelled| {
                let (reference, auth) = mixed_credentials
                    .pop_front()
                    .ok_or("The reviewed mixed rsync credentials are unavailable.")?;
                if reference != target.reference {
                    return Err("The mixed rsync route changed before review.".into());
                }
                let reviewed = Arc::clone(&mixed_review);
                job_executor::rsync::preview_mixed(
                    auth,
                    target,
                    trust,
                    Arc::clone(cancelled),
                    move || reviewed.open().map(|_| ()),
                )
            },
            move |reference, spec, review_vacant_server_upload, trust, cancelled| {
                let (expected, reviewed_spec, auth) = privileged_credentials
                    .pop_front()
                    .ok_or("The privileged rsync credentials are unavailable.")?;
                if &expected != reference || &reviewed_spec != spec {
                    return Err("The privileged rsync route or path changed before review.".into());
                }
                let routes = Arc::clone(&privileged_review);
                let mode = match (review_vacant_server_upload, &spec.operation) {
                    (
                        true,
                        kortty_server_archive::privileged_rsync::Operation::Upload {
                            directory: true,
                            ..
                        },
                    ) => PrivilegedPreviewMode::VacantTree,
                    (true, _) => PrivilegedPreviewMode::VacantFile,
                    (false, _) => PrivilegedPreviewMode::Standard,
                };
                job_executor::rsync::preview_privileged(
                    auth,
                    reference,
                    spec,
                    mode,
                    trust,
                    Arc::clone(cancelled),
                    move || routes.open().map(|_| ()),
                )
            },
            move || {
                completed_review.open().map_err(|error| error.to_string())?;
                let policy = ClipboardPolicy::load();
                if policy.problem.is_some()
                    || !policy.file_transfer_allowed
                    || !policy.minimum_isolation.is_none()
                    || completed_review.references.iter().any(|reference| {
                        policy
                            .server_access()
                            .first_blocked(reference.target.servers(reference.jump.as_ref()))
                            .is_some()
                    })
                {
                    return Err("Managed policy forbids this rsync review.".into());
                }
                Ok(())
            },
        )
        .map(move |message| Message::Scheduler(id, message))
    }
}

struct ReviewedRoutes {
    path: PathBuf,
    groups: Option<TargetGroups>,
    references: Vec<ConnectionRef>,
}

impl ReviewedRoutes {
    fn has_teamwork(&self) -> bool {
        self.references
            .iter()
            .any(|reference| reference.shared_identity().is_some())
    }

    fn open(&self) -> io::Result<ConnectionStore> {
        let changed = || {
            io::Error::other(
                "The saved rsync routes changed. Refresh the selection and review again.",
            )
        };
        let store = ConnectionStore::open(self.path.clone())?;
        let shared = if self.has_teamwork() {
            SharedTargets::load_reviewed(&store, self.references.iter().cloned())
                .map_err(io::Error::other)?
        } else {
            SharedTargets::default()
        };
        if self
            .references
            .iter()
            .any(|reference| shared.require_saved(&store, reference).is_err())
            || self.groups.as_ref().is_some_and(|groups| {
                !groups
                    .resolve(&store, &shared, false)
                    .is_ok_and(|resolved| resolved == self.references)
            })
        {
            return Err(changed());
        }
        Ok(store)
    }
}
