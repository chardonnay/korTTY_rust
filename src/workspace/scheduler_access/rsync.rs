use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use iced::{Task, window};

use crate::scheduler_window::{ConnectionRef, RemotePath, TargetGroups};

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
        let local_targets = request.local_existing_targets();
        let mixed_targets = request.mixed_targets();
        let privileged_targets = request.privileged_targets();
        let mut credentials = VecDeque::new();
        let mut mixed_credentials = VecDeque::new();
        let mut privileged_credentials = VecDeque::new();
        let reviewed = if targets.is_empty()
            && local_targets.is_empty()
            && mixed_targets.is_empty()
            && privileged_targets.is_empty()
        {
            None
        } else {
            let result = (|| -> Result<ReviewedRoutes, String> {
                let installed = ClipboardPolicy::load();
                if !self.scheduler_sync_allowed()
                    || installed.problem.is_some()
                    || !installed.file_transfer_allowed
                    || !installed.minimum_isolation.is_none()
                {
                    return Err("Managed policy forbids existing-directory rsync review.".into());
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
                    references: targets
                        .iter()
                        .map(|(reference, _)| reference)
                        .chain(local_targets.iter().map(|(reference, _)| reference))
                        .chain(mixed_targets.iter().map(|target| &target.reference))
                        .chain(privileged_targets.iter().map(|(reference, _)| reference))
                        .cloned()
                        .collect(),
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
                for (reference, destination) in targets {
                    let connection = store
                        .get(reference.id)
                        .ok_or("The saved rsync route is unavailable.")?;
                    let auth = job_executor::Credentials::resolve_directory_browser(
                        &reference,
                        connection,
                        installed.server_access(),
                        |id, kind, endpoint| self.vault_credential(id, kind, endpoint),
                    )?;
                    credentials.push_back((reference, destination, auth));
                }
                for target in &mixed_targets {
                    let reference = &target.reference;
                    let connection = store
                        .get(reference.id)
                        .ok_or("The saved mixed rsync route is unavailable.")?;
                    let auth = job_executor::Credentials::resolve_directory_browser(
                        reference,
                        connection,
                        installed.server_access(),
                        |id, kind, endpoint| self.vault_credential(id, kind, endpoint),
                    )?;
                    mixed_credentials.push_back((reference.clone(), auth));
                }
                for (reference, spec) in privileged_targets {
                    let connection = store
                        .get(reference.id)
                        .ok_or("The saved privileged rsync route is unavailable.")?;
                    let auth = job_executor::Credentials::resolve_directory_browser(
                        &reference,
                        connection,
                        installed.server_access(),
                        |id, kind, endpoint| self.vault_credential(id, kind, endpoint),
                    )?;
                    privileged_credentials.push_back((reference, spec, auth));
                }
                Ok(reviewed)
            })();
            match result {
                Ok(reviewed) => Some(Arc::new(reviewed)),
                Err(error) => return self.report_scheduler_group_error(id, error),
            }
        };
        let Some(view) = self.scheduler_views.get_mut(&id) else {
            return Task::none();
        };
        let local_review = if local_targets.is_empty() && mixed_targets.is_empty() {
            None
        } else {
            reviewed.clone()
        };
        let mixed_review = reviewed.clone();
        let privileged_review = reviewed.clone();
        view.begin_rsync_review(
            request,
            move |reference, destination: &RemotePath, trust, cancelled| {
                let (expected, path, auth) = credentials
                    .pop_front()
                    .ok_or("The reviewed rsync credentials are unavailable.")?;
                if &expected != reference || &path != destination {
                    return Err("The rsync route or receiver changed before review.".into());
                }
                let reviewed = reviewed
                    .as_ref()
                    .cloned()
                    .ok_or("The existing-directory review is unavailable.")?;
                job_executor::rsync::preview_existing(
                    auth,
                    reference,
                    destination,
                    trust,
                    Arc::clone(cancelled),
                    move || reviewed.open().map(|_| ()),
                )
            },
            move |target, trust, cancelled| {
                let (reference, auth) = mixed_credentials
                    .pop_front()
                    .ok_or("The reviewed mixed rsync credentials are unavailable.")?;
                if reference != target.reference {
                    return Err("The mixed rsync route changed before review.".into());
                }
                let reviewed = mixed_review
                    .as_ref()
                    .cloned()
                    .ok_or("The mixed rsync route review is unavailable.")?;
                job_executor::rsync::preview_mixed(
                    auth,
                    target,
                    trust,
                    Arc::clone(cancelled),
                    move || reviewed.open().map(|_| ()),
                )
            },
            move |reference, spec, trust, cancelled| {
                let (expected, reviewed_spec, auth) = privileged_credentials
                    .pop_front()
                    .ok_or("The privileged rsync credentials are unavailable.")?;
                if &expected != reference || &reviewed_spec != spec {
                    return Err("The privileged rsync route or path changed before review.".into());
                }
                let routes = privileged_review
                    .as_ref()
                    .cloned()
                    .ok_or("The privileged rsync review is unavailable.")?;
                job_executor::rsync::preview_privileged(
                    auth,
                    reference,
                    spec,
                    trust,
                    Arc::clone(cancelled),
                    move || routes.open().map(|_| ()),
                )
            },
            move || {
                if let Some(routes) = local_review {
                    routes.open().map_err(|error| error.to_string())?;
                    let policy = ClipboardPolicy::load();
                    if policy.problem.is_some()
                        || !policy.file_transfer_allowed
                        || !policy.minimum_isolation.is_none()
                        || routes.references.iter().any(|reference| {
                            policy
                                .server_access()
                                .first_blocked(reference.target.servers(reference.jump.as_ref()))
                                .is_some()
                        })
                    {
                        return Err(
                            "Managed policy forbids existing-directory rsync review.".into()
                        );
                    }
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
    fn open(&self) -> io::Result<ConnectionStore> {
        let changed = || {
            io::Error::other(
                "The saved rsync routes changed. Refresh the selection and review again.",
            )
        };
        let store = ConnectionStore::open(self.path.clone())?;
        if self.references.iter().any(|reference| {
            store.get(reference.id).map(ConnectionRef::from).as_ref() != Some(reference)
        }) || self.groups.as_ref().is_some_and(|groups| {
            !groups
                .resolve(&store, false)
                .is_ok_and(|resolved| resolved == self.references)
        }) {
            return Err(changed());
        }
        Ok(store)
    }
}
