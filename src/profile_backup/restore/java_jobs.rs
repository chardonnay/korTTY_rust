use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Write as _};
use std::path::Path;

use kortty_jobs::{JobGraph, PreparedJobAdditions, SchedulerStore, StoreLock};
use zeroize::Zeroizing;

use super::super::java_migration::SchedulerSource;
use super::{
    Directory, JOBS, JavaUpdates, Snapshot, data, invalid, java_credential_restore, java_scheduler,
    missing, read_bounded,
};
use crate::clipboard_policy::ClipboardPolicy;
use crate::connection_store::Store;
use crate::connection_store::import::{
    JavaImportedRoute, check_java_imported_routes, java_connection_candidate,
};
use crate::private_files::{FileStamp, require_single_link};
use crate::scheduler_window::{
    ConnectionGroup, ConnectionRef, SharedTargets, SshCommand, TargetGroups, imported_privileged,
};
use crate::snippets;

/// Local routes require a matching reviewed catalogue. Shared routes require active sources.
pub(super) fn stage(
    directory: &Directory,
    snapshot: &Snapshot,
    connections: Option<&java_credential_restore::Connections>,
    source: SchedulerSource,
    shared: &SharedTargets,
    policy: &ClipboardPolicy,
    notes: &mut Vec<String>,
) -> io::Result<Option<Zeroizing<Vec<u8>>>> {
    let source_omissions = source.review.omitted.clone();
    let Some(catalogue) = connections else {
        if source
            .target_review
            .as_ref()
            .is_some_and(|review| review.assignment_count() != 0)
        {
            return Err(invalid(
                "Saved connections changed. Open the backup again to review Scheduler targets.",
            ));
        }
        notes.push(format!(
            "Scheduler jobs omitted because no exact SSH routes matched ({})",
            source.review.jobs.len()
        ));
        return Ok(None);
    };
    let snippets = if source
        .review
        .jobs
        .iter()
        .any(|job| job.action.snippet_id().is_some())
    {
        snippet_links(directory, snapshot, None)?
    } else {
        Default::default()
    };
    let candidate = java_connection_candidate(&catalogue.contents)?;
    let selected = source
        .target_review
        .as_ref()
        .map(|review| review.bindings(catalogue.before, &candidate, policy))
        .transpose()?
        .unwrap_or_default();
    let resolve = |id: &str| {
        selected
            .get(id)
            .map(connection_reference)
            .or_else(|| resolve_target(catalogue, &source.teamwork_origins, shared, policy, id))
    };
    let bound = source.review.bind_all_scoped(
        resolve,
        |group| {
            let local_path = source
                .target_review
                .as_ref()
                .map_or(group, |review| review.group_path(group));
            let mut groups = Vec::new();
            let members = catalogue
                .groups
                .member_ids(group)?
                .into_iter()
                .map(|id| {
                    let route = resolve(id)?;
                    let native = route.shared_identity().map_or_else(
                        || ConnectionGroup::from(local_path),
                        |identity| ConnectionGroup::from_shared(identity.source(), group),
                    );
                    if !groups.contains(&native) {
                        groups.push(native);
                    }
                    Some((id.to_owned(), route))
                })
                .collect::<Option<Vec<_>>>()?;
            // Each source group must equal the restored membership.
            // Overlapping groups and explicit targets must not mask extra members.
            let expanded = TargetGroups {
                groups: groups.clone(),
                explicit: Vec::new(),
                group_order: Vec::new(),
            }
            .resolve(&candidate, shared, true)
            .ok()?;
            let same_members = expanded.len() == members.len()
                && expanded
                    .iter()
                    .all(|member| members.iter().any(|(_, route)| route == member));
            let preserve_archive_order =
                !expanded.iter().eq(members.iter().map(|(_, route)| route));
            same_members.then_some((groups, members, preserve_archive_order))
        },
        |id| {
            if let Some(route) = catalogue.groups.shared(id) {
                return route.rsync_folder.clone();
            }
            selected
                .get(id)
                .or_else(|| catalogue.routes.get(id))?
                .rsync_folder
                .clone()
        },
        |id| snippets.contains(&id),
    )?;
    for (reason, count) in bound.omitted {
        let added = count - source_omissions.get(&reason).copied().unwrap_or(0);
        if added > 0 {
            notes.push(format!("{} ({added})", reason.description()));
        }
    }
    if bound.jobs.jobs().is_empty() {
        return Ok(None);
    }
    check_routes(&candidate, &bound.jobs, policy, Some(shared))?;
    let current = read_jobs(directory)?;
    let additions = prepare_additions(current.as_ref(), &bound.jobs)?;
    for (count, label) in [
        (
            additions.unchanged,
            "Identical Scheduler jobs already saved",
        ),
        (
            additions.conflicts,
            "Scheduler jobs omitted due to local conflicts",
        ),
        (
            additions.missing_dependencies,
            "Scheduler jobs omitted due to missing prerequisites",
        ),
        (
            additions.full,
            "Scheduler jobs omitted due to storage limits",
        ),
    ] {
        if count > 0 {
            notes.push(format!("{label} ({count})"));
        }
    }
    if additions.added == 0 {
        return Ok(None);
    }
    notes.push(format!(
        "Disabled Scheduler jobs ready for additive restore (up to {})",
        additions.added
    ));
    let jobs = java_scheduler::encode(&bound.jobs)?;
    Ok(Some(jobs))
}

fn resolve_target(
    catalogue: &java_credential_restore::Connections,
    origins: &std::collections::BTreeMap<String, crate::connection_store::teamwork::Origin>,
    shared: &SharedTargets,
    policy: &ClipboardPolicy,
    id: &str,
) -> Option<ConnectionRef> {
    if let Some(route) = catalogue.groups.shared(id) {
        return shared
            .java_target(
                origins.get(&route.source)?,
                id,
                &route.group,
                &route.target,
                route.jump.as_ref(),
            )
            .filter(|reference| reference.require_shared_policy(policy).is_ok())
            .cloned();
    }
    catalogue.routes.get(id).map(connection_reference)
}

fn connection_reference(route: &JavaImportedRoute) -> ConnectionRef {
    ConnectionRef {
        teamwork: None,
        temporary_keys: None,
        id: route.id,
        target: route.target.clone(),
        jump: route.jump.clone(),
    }
}

fn snippet_links(
    directory: &Directory,
    snapshot: &Snapshot,
    prepared: Option<&JavaUpdates<'_>>,
) -> io::Result<HashSet<snippets::SnippetId>> {
    let Some(incoming) = snapshot.get(snippets::FILE_NAME) else {
        return Ok(Default::default());
    };
    if let Some(merged) = prepared.and_then(|updates| {
        updates
            .catalogues
            .iter()
            .find_map(|(path, bytes)| (*path == snippets::FILE_NAME).then_some(bytes))
    }) {
        return snippets::java_scheduler_snippets(incoming, merged);
    }
    let current = missing(directory.open_file(snippets::FILE_NAME))?
        .map(|file| read_bounded(file, data::file_limit(snippets::FILE_NAME)))
        .transpose()?;
    if prepared.is_none()
        && let Some(merged) = snippets::merge_java_backup(
            current.as_deref().map(Vec::as_slice),
            incoming,
            snippets::custom_script_headers_allowed(),
        )?
    {
        return snippets::java_scheduler_snippets(incoming, &merged);
    }
    current.map_or_else(
        || Ok(Default::default()),
        |current| snippets::java_scheduler_snippets(incoming, &current),
    )
}

fn linked_snippet(action: &SshCommand) -> Option<snippets::SnippetId> {
    action.snippet.as_ref().map(|link| link.id).or_else(|| {
        match &action.imported_privileged.as_deref()?.settings {
            imported_privileged::Settings::Snippet { link, .. } => Some(link.id),
            _ => None,
        }
    })
}

fn check_routes(
    candidate: &Store,
    jobs: &JobGraph<SshCommand>,
    policy: &ClipboardPolicy,
    shared: Option<&SharedTargets>,
) -> io::Result<()> {
    let routes: Vec<_> = jobs
        .jobs()
        .iter()
        .flat_map(|job| job.action().targets())
        .filter(|route| route.shared_identity().is_none())
        .map(|route| JavaImportedRoute {
            id: route.id,
            target: route.target.clone(),
            jump: route.jump.clone(),
            rsync_folder: None,
        })
        .collect();
    check_java_imported_routes(candidate, &routes, policy.server_access())?;
    for job in jobs.jobs() {
        if let Some(shared) = shared {
            job.action()
                .require_current_targets(candidate, shared)
                .map_err(invalid)?;
        } else if !job.action().has_teamwork_targets() {
            job.action()
                .require_current_target_groups(candidate)
                .map_err(invalid)?;
        }
    }
    Ok(())
}

fn read_jobs(directory: &Directory) -> io::Result<Option<(FileStamp, Zeroizing<Vec<u8>>)>> {
    let Some(file) = missing(directory.open_file(JOBS))? else {
        return Ok(None);
    };
    require_single_link(&file)?;
    let stamp = FileStamp::read(&file)?;
    let bytes = read_bounded(file.try_clone()?, data::file_limit(JOBS))?;
    stamp.require_unchanged(&file)?;
    Ok(Some((stamp, bytes)))
}

fn prepare_additions(
    current: Option<&(FileStamp, Zeroizing<Vec<u8>>)>,
    jobs: &JobGraph<SshCommand>,
) -> io::Result<PreparedJobAdditions> {
    let bytes = current.map(|(_, bytes)| bytes.as_slice());
    SchedulerStore::prepare_disabled_additions(bytes, jobs).map_err(io::Error::other)
}

pub(super) struct Pending {
    _lock: StoreLock,
    before: Option<FileStamp>,
    candidate: Option<Zeroizing<Vec<u8>>>,
}

impl Pending {
    pub(super) fn prepare(
        root: &Path,
        directory: &Directory,
        snapshot: &Snapshot,
        updates: &JavaUpdates<'_>,
        connections: &[u8],
        jobs: JobGraph<SshCommand>,
        initial_policy: Option<&ClipboardPolicy>,
    ) -> io::Result<Self> {
        let candidate = java_connection_candidate(connections)?;
        let shared = if initial_policy.is_some()
            && jobs
                .jobs()
                .iter()
                .any(|job| job.action().has_teamwork_targets())
        {
            Some(
                SharedTargets::load_reviewed(
                    &candidate,
                    jobs.jobs()
                        .iter()
                        .flat_map(|job| job.action().targets())
                        .cloned(),
                )
                .map_err(invalid)?,
            )
        } else {
            None
        };
        // Once publication starts, replay the disabled definitions. Their
        // launch checks the current policy and source membership again.
        let replay_policy = ClipboardPolicy::default();
        check_routes(
            &candidate,
            &jobs,
            initial_policy.unwrap_or(&replay_policy),
            shared.as_ref(),
        )?;
        let links: Vec<_> = jobs
            .jobs()
            .iter()
            .filter_map(|job| linked_snippet(job.action()))
            .collect();
        if !links.is_empty() {
            let snippets = snippet_links(directory, snapshot, Some(updates))?;
            if links.iter().any(|id| !snippets.contains(id)) {
                return Err(invalid(
                    "A reviewed Java Scheduler snippet or its variables changed.",
                ));
            }
        }
        let lock = StoreLock::acquire(&root.join(JOBS)).map_err(io::Error::other)?;
        let current = read_jobs(directory)?;
        let additions = prepare_additions(current.as_ref(), &jobs)?;
        Ok(Self {
            _lock: lock,
            before: current.map(|(stamp, _)| stamp),
            candidate: additions.bytes.map(Zeroizing::new),
        })
    }

    pub(super) fn publish(self, directory: &Directory) -> io::Result<()> {
        let Some(candidate) = self.candidate else {
            return Ok(());
        };
        let write = |file: &mut File| file.write_all(&candidate);
        if let Some(before) = self.before {
            directory.replace_with(JOBS, write, || {
                let current = directory.open_file(JOBS)?;
                require_single_link(&current)?;
                before.require_unchanged(&current)
            })?;
        } else {
            directory.publish_with(JOBS, write)?;
        }
        directory.sync()
    }
}
