use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::AtomicBool};

use kortty_server_archive::privileged_rsync::{Review as PrivilegedReview, Spec as PrivilegedSpec};
use kortty_server_archive::rsync_tree::ExistingTree;
use serde::{Deserialize, Serialize};

use super::{
    Approval, ConnectionRef, DirectoryReview, MixedReceiver, MixedReview, MixedReviewTarget,
    RemotePath, ReviewRequest, Transfer,
};
use crate::job_executor::rsync::LocalExistingSnapshot;
use crate::sftp_window::LocalFileSnapshot;

const TARGETS: &str = "Choose one exact rsync destination for each saved SSH target.";
const REVIEW: &str = "Review every rsync destination and saved SSH route before running.";
const OVERLAP: &str = "Choose separate, non-overlapping local destinations for the rsync targets.";

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Route {
    pub(crate) profile: u64,
    pub(crate) destination: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) approval: Option<Approval>,
}

impl Transfer {
    pub(crate) fn for_target(&self, index: usize, upload: bool) -> Result<Self, &'static str> {
        let route = index
            .checked_sub(1)
            .map(|index| self.routes.get(index).ok_or(TARGETS))
            .transpose()?;
        Ok(Self {
            program: self.program.clone(),
            local: match route {
                Some(route) if !upload => PathBuf::from(&route.destination),
                _ => self.local.clone(),
            },
            remote: match route {
                Some(route) if upload => RemotePath::try_from(route.destination.clone())?,
                _ => self.remote.clone(),
            },
            directory: self.directory,
            existing: self.existing,
            existing_download: self.existing_download,
            mixed: self.mixed.clone(),
            additional_sources: self.additional_sources.clone(),
            approval: match route {
                Some(route) => route.approval.clone(),
                None => self.approval.clone(),
            },
            standing: self.standing.clone(),
            elevation: self.elevation,
            routes: Vec::new(),
        })
    }

    pub(crate) fn validate_routes(
        &self,
        primary: &ConnectionRef,
        additional: &[ConnectionRef],
        upload: bool,
    ) -> Result<(), &'static str> {
        if self.routes.len() != additional.len() {
            return Err(TARGETS);
        }
        if additional.is_empty()
            && !self.uses_existing_destination()
            && !self.uses_passwordless_sudo()
        {
            return Ok(());
        }
        let mut local_destinations = Vec::new();
        for (index, reference) in std::iter::once(primary).chain(additional).enumerate() {
            if index > 0 && self.routes[index - 1].profile != reference.id {
                return Err(TARGETS);
            }
            let transfer = self.for_target(index, upload)?;
            transfer.validate(upload)?;
            if let Some(approval) = &transfer.approval
                && (&approval.reference != reference
                    || approval.program != self.program
                    || (!upload
                        && !additional.is_empty()
                        && !transfer.directory
                        && transfer.additional_sources.is_empty()
                        && approval.download.is_none()))
            {
                return Err(REVIEW);
            }
            if !upload {
                let effective = if transfer.existing_download.is_some() {
                    transfer.existing_local_destination()?
                } else {
                    transfer.local
                };
                push_disjoint(&mut local_destinations, &effective)?;
            }
        }
        Ok(())
    }

    pub(crate) fn require_group_review(&self) -> Result<(), &'static str> {
        if self.routes.is_empty()
            && !self.uses_existing_destination()
            && !self.uses_passwordless_sudo()
        {
            return Ok(());
        }
        let primary = self.approval.as_ref().ok_or(REVIEW)?;
        if !self.review_matches_mode(primary)
            || self.routes.iter().any(|route| {
                route.approval.as_ref().is_none_or(|review| {
                    review.digest != primary.digest
                        || !self.review_matches_mode(review)
                        || !primary.same_existing_source(review)
                })
            })
        {
            return Err(REVIEW);
        }
        if !local_receivers_distinct(
            std::iter::once(primary).chain(
                self.routes
                    .iter()
                    .filter_map(|route| route.approval.as_ref()),
            ),
        ) {
            return Err(OVERLAP);
        }
        Ok(())
    }

    fn review_matches_mode(&self, approval: &Approval) -> bool {
        if self.mixed.is_some() {
            approval
                .mixed
                .as_ref()
                .is_some_and(|review| self.uses_passwordless_sudo() == review.privileged.is_some())
        } else {
            self.uses_passwordless_sudo() == approval.privileged.is_some()
                && approval.mixed.is_none()
                && approval.existing.is_some() == self.existing.is_some()
                && approval.existing_download.is_some()
                    == (self.directory && self.existing_download.is_some())
                && approval.existing_file_download.is_some()
                    == (!self.directory && self.existing_download.is_some())
        }
    }
}

fn push_disjoint(paths: &mut Vec<PathBuf>, destination: &Path) -> Result<(), &'static str> {
    let path = super::local_scope_path(destination)?;
    if paths
        .iter()
        .any(|previous| previous.starts_with(&path) || path.starts_with(previous))
    {
        return Err(OVERLAP);
    }
    paths.push(path);
    Ok(())
}

fn local_receivers_distinct<'a>(approvals: impl Iterator<Item = &'a Approval>) -> bool {
    let mut seen = Vec::<LocalWitness<'_>>::new();
    for snapshot in
        approvals.filter_map(|approval| approval.local_review().and_then(|(_, snapshot)| snapshot))
    {
        if seen.iter().any(|previous| previous.aliases(&snapshot)) {
            return false;
        }
        seen.push(snapshot);
    }
    true
}

enum LocalWitness<'a> {
    Tree(&'a LocalExistingSnapshot),
    File(&'a LocalFileSnapshot),
}

impl LocalWitness<'_> {
    fn aliases(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Tree(a), Self::Tree(b)) => a.aliases(b),
            (Self::File(a), Self::File(b)) => a.aliases(b),
            _ => false,
        }
    }
}

impl Approval {
    fn local_review(&self) -> Option<(&Path, Option<LocalWitness<'_>>)> {
        match self.mixed.as_deref().map(|review| &review.receiver) {
            Some(MixedReceiver::Local {
                destination,
                snapshot,
            }) => Some((destination, snapshot.as_ref().map(LocalWitness::Tree))),
            _ => self
                .existing_download
                .as_ref()
                .map(|review| {
                    (
                        review.destination.as_path(),
                        Some(LocalWitness::Tree(&review.snapshot)),
                    )
                })
                .or_else(|| {
                    self.existing_file_download.as_ref().map(|review| {
                        (
                            review.destination.as_path(),
                            Some(LocalWitness::File(&review.snapshot)),
                        )
                    })
                }),
        }
    }
}

pub(crate) struct ReviewRequests {
    pub(super) primary: ReviewRequest,
    pub(super) additional: Vec<ReviewRequest>,
}

#[derive(Clone)]
pub struct Reviewed {
    pub(super) primary: Approval,
    pub(super) additional: Vec<Approval>,
}

impl Reviewed {
    pub(in crate::scheduler_window) fn restore(transfer: &Transfer) -> Option<Self> {
        Some(Self {
            primary: transfer.approval.clone()?,
            additional: transfer
                .routes
                .iter()
                .map(|route| route.approval.clone())
                .collect::<Option<Vec<_>>>()?,
        })
    }

    pub(in crate::scheduler_window) fn apply(&self, transfer: &mut Transfer) {
        transfer.approval = Some(self.primary.clone());
        for (route, approval) in transfer.routes.iter_mut().zip(&self.additional) {
            route.approval = Some(approval.clone());
        }
    }
}

impl ReviewRequests {
    pub(in crate::scheduler_window) fn for_transfer(
        transfer: &Transfer,
        primary: &ConnectionRef,
        additional: &[ConnectionRef],
        upload: bool,
    ) -> Result<Self, &'static str> {
        transfer.validate_routes(primary, additional, upload)?;
        let mut requests = std::iter::once(primary).chain(additional).enumerate().map(
            |(index, reference)| -> Result<ReviewRequest, &'static str> {
                let transfer = transfer.for_target(index, upload)?;
                transfer.validate(upload)?;
                if transfer.mixed.is_some() {
                    let mixed = transfer.mixed_target(reference, upload)?;
                    return Ok(ReviewRequest {
                        program: transfer.program,
                        reference: reference.clone(),
                        upload: None,
                        directory: None,
                        directory_upload: None,
                        existing: None,
                        existing_download: None,
                        mixed: Some(mixed),
                        batch: None,
                        download: None,
                        privileged: None,
                    });
                }
                let batch = transfer.batch_plan(upload)?;
                let privileged = transfer
                    .uses_passwordless_sudo()
                    .then(|| transfer.privileged_spec(upload))
                    .transpose()?;
                let existing = transfer
                    .existing
                    .map(|settings| {
                        transfer
                            .existing_destination()
                            .map(|destination| (settings, destination))
                    })
                    .transpose()?;
                let existing_download = transfer
                    .existing_download
                    .map(|settings| {
                        transfer
                            .existing_local_destination()
                            .map(|destination| (settings, destination))
                    })
                    .transpose()?;
                let single = batch.is_none();
                let download = (single && !upload).then(|| DirectoryReview {
                    source: transfer.remote.clone(),
                    destination: transfer.local.clone(),
                });
                let (directory, download) = if transfer.directory {
                    (download, None)
                } else {
                    (None, download)
                };
                Ok(ReviewRequest {
                    program: transfer.program,
                    reference: reference.clone(),
                    upload: (single && upload && !transfer.directory)
                        .then(|| (transfer.local.clone(), transfer.remote.clone())),
                    directory,
                    download,
                    directory_upload: (single && upload && transfer.directory)
                        .then_some((transfer.local, transfer.remote)),
                    existing,
                    existing_download,
                    mixed: None,
                    batch,
                    privileged,
                })
            },
        );
        Ok(Self {
            primary: requests.next().expect("one primary target")?,
            additional: requests.collect::<Result<Vec<_>, _>>()?,
        })
    }

    pub(in crate::scheduler_window) fn matches(&self, approval: &Reviewed) -> bool {
        self.primary
            .matches(&approval.primary, self.additional.is_empty())
            && self.additional.len() == approval.additional.len()
            && local_receivers_distinct(
                std::iter::once(&approval.primary).chain(&approval.additional),
            )
            && self
                .additional
                .iter()
                .zip(&approval.additional)
                .all(|(request, route)| {
                    request.matches(route, false)
                        && route.digest == approval.primary.digest
                        && approval.primary.same_existing_source(route)
                })
    }

    pub(crate) fn existing_targets(&self) -> Vec<(ConnectionRef, RemotePath)> {
        std::iter::once(&self.primary)
            .chain(&self.additional)
            .filter(|request| request.privileged.is_none())
            .filter_map(|request| {
                let (_, destination) = request.existing.as_ref()?;
                Some((request.reference.clone(), destination.clone()))
            })
            .collect()
    }

    pub(crate) fn local_existing_targets(&self) -> Vec<(ConnectionRef, PathBuf)> {
        std::iter::once(&self.primary)
            .chain(&self.additional)
            .filter(|request| request.privileged.is_none())
            .filter_map(|request| {
                let (_, destination) = request.existing_download.as_ref()?;
                Some((request.reference.clone(), destination.clone()))
            })
            .collect()
    }

    pub(crate) fn mixed_targets(&self) -> Vec<MixedReviewTarget> {
        std::iter::once(&self.primary)
            .chain(&self.additional)
            .filter_map(|request| request.mixed.clone())
            .collect()
    }

    pub(crate) fn privileged_targets(&self) -> Vec<(ConnectionRef, PrivilegedSpec)> {
        std::iter::once(&self.primary)
            .chain(&self.additional)
            .filter_map(|request| {
                let spec = request.privileged.clone()?;
                Some((request.reference.clone(), spec))
            })
            .collect()
    }

    pub(crate) fn review(
        self,
        mut inspect: impl FnMut(
            &ConnectionRef,
            &RemotePath,
            &kortty_ssh::TrustStore,
            &Arc<AtomicBool>,
        ) -> Result<ExistingTree, String>,
        mut inspect_mixed: impl FnMut(
            &MixedReviewTarget,
            &kortty_ssh::TrustStore,
            &Arc<AtomicBool>,
        ) -> Result<MixedReview, String>,
        mut inspect_privileged: impl FnMut(
            &ConnectionRef,
            &PrivilegedSpec,
            &kortty_ssh::TrustStore,
            &Arc<AtomicBool>,
        ) -> Result<PrivilegedReview, String>,
        cancelled: &Arc<AtomicBool>,
    ) -> Result<Reviewed, String> {
        super::require_review_running(cancelled)?;
        super::super::remote::validate_local_path(&self.primary.program)?;
        let trust = crate::ssh_connection::trust_store().map_err(|error| error.to_string())?;
        let digest = crate::job_executor::rsync::program_digest(&self.primary.program)
            .map_err(|error| error.to_string())?;
        let primary = super::review(
            self.primary,
            &trust,
            digest,
            &mut inspect,
            &mut inspect_mixed,
            &mut inspect_privileged,
            cancelled,
        )?;
        let additional = self
            .additional
            .into_iter()
            .map(|request| {
                let review = super::review(
                    request,
                    &trust,
                    digest,
                    &mut inspect,
                    &mut inspect_mixed,
                    &mut inspect_privileged,
                    cancelled,
                )?;
                if !primary.same_existing_source(&review) {
                    return Err("The local rsync source changed between route reviews.".to_owned());
                }
                Ok(review)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let routes = || std::iter::once(&primary).chain(&additional);
        if !local_receivers_distinct(routes()) {
            return Err(OVERLAP.into());
        }
        let mut canonical_destinations = Vec::new();
        if primary.local_review().is_some() {
            let program = primary
                .program
                .canonicalize()
                .map_err(|_| "The approved rsync executable changed or is unavailable.")?;
            for (destination, snapshot) in routes().filter_map(Approval::local_review) {
                super::require_review_running(cancelled)?;
                let canonical = super::canonical_local_receiver(destination, snapshot.is_some())?;
                super::require_program_outside_receiver(&program, &canonical)?;
                push_disjoint(&mut canonical_destinations, &canonical)?;
            }
        }
        Ok(Reviewed {
            primary,
            additional,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use kortty_jobs::{Job, JobId};
    use serde_json::json;

    use crate::scheduler_window::{RemoteAction, SshCommand, form::Draft};

    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    #[test]
    fn grouped_rsync_file_downloads_refuse_two_names_for_the_same_local_receiver() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let program = root.join("rsync");
        let first = json!({"id": 1, "target": {"host": "first.example", "port": 22, "username": "operator"}});
        let second = json!({"id": 2, "target": {"host": "second.example", "port": 22, "username": "operator"}});
        let first_path = root.join("one/report.txt");
        let second_path = root.join("two/report.txt");
        let source = "/site/report.txt";
        let mut manifest = kortty_server_archive::rsync_tree::Manifest::default();
        manifest.add("", Some((4, [1; 32]))).unwrap();
        let inventory = manifest.finish();
        let review = |reference, destination, identity: u8| {
            json!({
                "program": program, "digest": vec![8; 32], "reference": reference, "pins": ["SHA256:fixture"],
                "download": {"source": source, "destination": destination},
                "existing_file_download": {"destination": destination, "snapshot": {
                    "inventory": inventory, "fingerprint": vec![identity + 2; 32], "root_identity": vec![identity; 32],
                }},
            })
        };
        let action = |identity| {
            serde_json::from_value::<SshCommand>(json!({
                "connection": first, "additional_connections": [second],
                "remote": {"kind": "rsync_download", "program": program, "local": first_path, "remote": source,
                    "existing_download": {"delete": false, "include_source_name": false},
                    "approval": review(&first, &first_path, 1),
                    "routes": [{"profile": 2, "destination": second_path, "approval": review(&second, &second_path, identity)}],
                },
            }))
            .unwrap()
            .validated()
            .unwrap()
        };
        assert!(action(2).require_executable().is_ok());
        assert!(
            action(1)
                .require_executable()
                .unwrap_err()
                .contains("non-overlapping")
        );
    }

    #[test]
    fn existing_named_download_reviews_only_disjoint_effective_local_roots() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let program = root.join("rsync");
        let first = json!({"id": 1, "target": {"host": "first.example", "port": 22, "username": "operator"}});
        let second = json!({"id": 2, "target": {"host": "second.example", "port": 22, "username": "operator"}});
        let settings = json!({"delete": true, "include_source_name": true});
        let inventory = kortty_server_archive::rsync_tree::Manifest::default().finish();
        let first_root = root.join("first");
        let effective_first = first_root.join("documents");
        let second_root = first_root.join("adjacent");
        let effective_second = second_root.join("documents");
        let approval = |reference: &serde_json::Value, receiver: &Path, identity: u8| {
            json!({
                "program": program, "digest": vec![1u8; 32], "reference": reference, "pins": ["SHA256:fixture"],
                "directory": {"source": "/site/documents", "destination": receiver},
                "existing_download": {"destination": receiver.join("documents"), "settings": settings,
                    "snapshot": {"inventory": inventory, "fingerprint": vec![identity + 2; 32], "root_identity": vec![identity; 32]}},
            })
        };
        let saved = json!({
            "connection": first, "additional_connections": [second],
            "remote": {
                "kind": "rsync_download", "program": program, "local": first_root, "remote": "/site/documents",
                "directory": true, "existing_download": settings, "approval": approval(&first, &first_root, 1),
                "routes": [{"profile": 2, "destination": second_root, "approval": approval(&second, &second_root, 2)}],
            },
        });
        let decode = |value| serde_json::from_value::<SshCommand>(value).unwrap();
        let action = decode(saved.clone()).validated().unwrap();
        assert!(action.require_executable().is_ok());
        let id = JobId::new("local-existing-rsync").unwrap();
        let job = Job::new(id.clone(), "Grouped rsync download", action)
            .unwrap()
            .with_enabled(false);
        let draft = Draft::from_job(&job);
        let rebuilt = draft.build(id).unwrap();
        assert!(rebuilt.action().require_executable().is_ok());
        let requests = draft.rsync_candidate().unwrap();
        assert!(requests.existing_targets().is_empty());
        let paths = requests.local_existing_targets();
        assert_eq!(
            paths
                .iter()
                .map(|(route, path)| (route.id, path.as_path()))
                .collect::<Vec<_>>(),
            [
                (1, effective_first.as_path()),
                (2, effective_second.as_path())
            ]
        );

        let mut nested = saved.clone();
        let nested_root = effective_first.join("nested");
        nested["remote"]["routes"][0]["destination"] = json!(nested_root);
        nested["remote"]["routes"][0]["approval"] = approval(&second, &nested_root, 2);
        assert!(
            decode(nested)
                .validated()
                .err()
                .unwrap()
                .contains("non-overlapping")
        );

        let mut aliased = saved.clone();
        aliased["remote"]["routes"][0]["approval"]["existing_download"]["snapshot"]["root_identity"] =
            json!(vec![1u8; 32]);
        assert!(
            decode(aliased)
                .validated()
                .unwrap()
                .require_executable()
                .unwrap_err()
                .contains("non-overlapping")
        );

        let mut contains_program = saved.clone();
        contains_program["remote"]["program"] = json!(effective_first.join("bin/rsync"));
        contains_program["remote"]["approval"] = serde_json::Value::Null;
        assert!(
            decode(contains_program)
                .validated()
                .err()
                .unwrap()
                .contains("contain the rsync executable")
        );

        let mut downgraded = saved.clone();
        downgraded["remote"]
            .as_object_mut()
            .unwrap()
            .remove("existing_download");
        assert!(decode(downgraded).validated().is_err());

        let mut missing_witness = saved;
        missing_witness["remote"]["routes"][0]["approval"] = serde_json::Value::Null;
        assert!(
            decode(missing_witness)
                .validated()
                .unwrap()
                .require_executable()
                .is_err()
        );
    }

    #[test]
    fn existing_named_upload_binds_independent_witnesses_to_every_group_receiver() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let program = root.join("rsync");
        let source = root.join("documents");
        let first = json!({"id": 1, "target": {"host": "first.example", "port": 22, "username": "operator"}});
        let second = json!({"id": 2, "target": {"host": "second.example", "port": 22, "username": "operator"}});
        let settings = json!({"delete": false, "include_source_name": true});
        let inventory = kortty_server_archive::rsync_tree::Manifest::default().finish();
        let approval = |reference: &serde_json::Value, destination: &str, witness| {
            json!({
                "program": program, "digest": vec![1u8; 32], "reference": reference, "pins": ["SHA256:fixture"],
                "directory_upload": {"source": source, "destination": destination, "inventory": inventory},
                "existing": {"destination": format!("{destination}/documents"), "settings": settings,
                    "snapshot": {"inventory": inventory, "fingerprint": vec![witness; 32]}},
            })
        };
        let saved = json!({
            "connection": first, "additional_connections": [second],
            "remote": {
                "kind": "rsync_upload", "program": program, "local": source, "remote": "/one",
                "directory": true, "existing": settings, "approval": approval(&first, "/one", 2),
                "routes": [{"profile": 2, "destination": "/two", "approval": approval(&second, "/two", 3)}],
            },
        });
        let decode = |value| serde_json::from_value::<SshCommand>(value).unwrap();
        let action = decode(saved.clone()).validated().unwrap();
        assert!(action.require_executable().is_ok());
        let id = JobId::new("existing-rsync").unwrap();
        let job = Job::new(id.clone(), "Grouped rsync", action)
            .unwrap()
            .with_enabled(false);
        let draft = Draft::from_job(&job);
        let restored = draft.build(id).unwrap();
        let RemoteAction::RsyncUpload(transfer) = restored.action().remote_action().unwrap() else {
            panic!("expected grouped rsync upload")
        };
        let requests = draft.rsync_candidate().unwrap().existing_targets();
        assert_eq!(
            requests
                .iter()
                .map(|(route, path)| (route.id, path.as_str()))
                .collect::<Vec<_>>(),
            [(1, "/one/documents"), (2, "/two/documents")]
        );
        for (index, witness) in [2, 3].into_iter().enumerate() {
            let route = transfer.for_target(index, true).unwrap();
            assert_eq!(
                route
                    .approval
                    .unwrap()
                    .existing
                    .unwrap()
                    .snapshot
                    .fingerprint,
                [witness; 32]
            );
        }

        for (pointer, value) in [
            ("/remote/existing/delete", json!(true)),
            ("/remote/existing/include_source_name", json!(false)),
            ("/remote/approval/existing/destination", json!("/one")),
            ("/remote/routes/0/destination", json!("/third")),
            ("/remote/additional_sources", json!(["/more"])),
        ] {
            let mut changed = saved.clone();
            if let Some(field) = changed.pointer_mut(pointer) {
                *field = value;
            } else {
                changed["remote"]["additional_sources"] = value;
            }
            assert!(decode(changed).validated().is_err(), "accepted {pointer}");
        }
        let mut unreviewed = saved;
        let mut changed_source = unreviewed.clone();
        changed_source["remote"]["routes"][0]["approval"]["directory_upload"]["inventory"]["digest"] =
            json!(vec![6u8; 32]);
        assert!(
            decode(changed_source)
                .validated()
                .unwrap()
                .require_executable()
                .is_err()
        );
        unreviewed["remote"]["routes"][0]["approval"] = serde_json::Value::Null;
        assert!(
            decode(unreviewed)
                .validated()
                .unwrap()
                .require_executable()
                .is_err()
        );
    }

    #[test]
    fn grouped_approvals_bind_exact_download_paths_and_survive_reopening() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let program = root.join("rsync");
        let primary = json!({
            "id": 1, "target": {"host": "first.example", "port": 22, "username": "operator"},
        });
        let additional = json!({
            "id": 2, "target": {"host": "second.example", "port": 22, "username": "operator"},
        });
        let first_destination = root.join("first");
        let second_destination = root.join("second");
        let digest = [1; 32];
        let approval = |reference: &serde_json::Value, destination: &std::path::Path| {
            json!({
                "program": program, "digest": digest, "reference": reference, "pins": ["SHA256:fixture"],
                "download": { "source": "/incoming/file", "destination": destination },
            })
        };
        let saved = json!({
            "connection": primary, "additional_connections": [additional],
            "remote": {
                "kind": "rsync_download", "program": program, "local": first_destination, "remote": "/incoming/file",
                "approval": approval(&primary, &first_destination),
                "routes": [{
                    "profile": 2, "destination": second_destination,
                    "approval": approval(&additional, &second_destination),
                }],
            },
        });
        let decode = |value| serde_json::from_value::<SshCommand>(value).unwrap();
        let action = decode(saved.clone()).validated().unwrap();
        assert!(action.require_executable().is_ok());

        let mut wrong = saved.clone();
        wrong["remote"]["routes"][0]["destination"] = json!(root.join("different"));
        assert!(decode(wrong).validated().is_err());

        let mut wrong = saved.clone();
        wrong["remote"]["routes"][0]["approval"]["reference"] = primary;
        assert!(decode(wrong).validated().is_err());

        let mut collision = saved;
        collision["remote"]["routes"][0]["destination"] = json!(root.join("FIRST"));
        collision["remote"]["routes"][0]["approval"] = serde_json::Value::Null;
        assert!(decode(collision).validated().is_err());

        let id = JobId::new("grouped-rsync").unwrap();
        let job = Job::new(id.clone(), "Grouped rsync", action).unwrap();
        let mut editor = Draft::from_job(&job);
        let restored = editor.build(id.clone()).unwrap();
        assert!(restored.action().require_executable().is_ok());
        assert!(editor.set_rsync_destination(2, root.join("edited").to_str().unwrap().into()));
        let edited = editor.build(id).unwrap();
        assert!(edited.action().require_executable().is_err());
        let RemoteAction::RsyncDownload(transfer) = edited.action().remote.as_ref().unwrap() else {
            panic!("expected a grouped rsync download");
        };
        assert_eq!(
            transfer.for_target(1, false).unwrap().local,
            root.join("edited")
        );
    }
}
