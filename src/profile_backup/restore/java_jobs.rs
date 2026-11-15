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
use crate::scheduler_window::{ConnectionRef, SshCommand};
use crate::snippets;

/// Resolves source identities only against the connections this restore adds.
pub(super) fn stage(
    directory: &Directory,
    snapshot: &Snapshot,
    connections: Option<&java_credential_restore::Connections>,
    source: SchedulerSource,
    notes: &mut Vec<String>,
) -> io::Result<Option<Zeroizing<Vec<u8>>>> {
    let policy = ClipboardPolicy::load();
    let source_omissions = source.review.omitted.clone();
    let Some(catalogue) = connections else {
        notes.push(format!(
            "Scheduler jobs omitted because no new SSH routes matched ({})",
            source.review.jobs.len()
        ));
        return Ok(None);
    };
    let snippets = if source
        .review
        .jobs
        .iter()
        .any(|job| matches!(&job.action, java_scheduler::Action::Snippet { .. }))
    {
        snippet_links(directory, snapshot, None)?
    } else {
        Default::default()
    };
    let candidate = java_connection_candidate(&catalogue.contents)?;
    let bound = source.review.bind_all(
        |id| catalogue.routes.get(id).map(connection_reference),
        |group| {
            let routes = catalogue.groups.resolve(group, &catalogue.routes)?;
            // Check each group independently. An explicit target or overlapping group
            // can mask a membership difference in the combined list.
            candidate
                .in_group(group)
                .map(|connection| connection.id)
                .eq(routes.iter().map(|(_, route)| route.id))
                .then(|| {
                    routes
                        .into_iter()
                        .map(|(source_id, route)| {
                            (source_id.to_owned(), connection_reference(route))
                        })
                        .collect()
                })
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
    check_routes(&candidate, &bound.jobs, &policy)?;
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

fn connection_reference(route: &JavaImportedRoute) -> ConnectionRef {
    ConnectionRef {
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

fn check_routes(
    candidate: &Store,
    jobs: &JobGraph<SshCommand>,
    policy: &ClipboardPolicy,
) -> io::Result<()> {
    let routes: Vec<_> = jobs
        .jobs()
        .iter()
        .flat_map(|job| job.action().targets())
        .map(|route| JavaImportedRoute {
            id: route.id,
            target: route.target.clone(),
            jump: route.jump.clone(),
        })
        .collect();
    check_java_imported_routes(candidate, &routes, policy.server_access())?;
    for job in jobs.jobs() {
        job.action()
            .require_current_target_groups(candidate)
            .map_err(invalid)?;
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
        policy: &ClipboardPolicy,
    ) -> io::Result<Self> {
        check_routes(&java_connection_candidate(connections)?, &jobs, policy)?;
        if jobs.jobs().iter().any(|job| job.action().snippet.is_some()) {
            let snippets = snippet_links(directory, snapshot, Some(updates))?;
            if jobs
                .jobs()
                .iter()
                .filter_map(|job| job.action().snippet.as_ref())
                .any(|link| !snippets.contains(&link.id))
            {
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
