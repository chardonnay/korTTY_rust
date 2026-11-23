//! Parses portable job definitions from Java KorTTY's Scheduler repository.

mod targets;
pub(crate) use targets::Review as TargetReview;

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::num::NonZeroU32;
use std::path::PathBuf;

use kortty_jobs::{
    Date, DependencyMode, Job, JobDefinition, JobGraph, JobId, JobSchedule, MAX_DEPENDENCIES,
    MAX_JOBS, MAX_SWARM_PARALLELISM, MAX_SWARM_TARGETS, SchedulerStore, Time, Weekday,
};
use roxmltree::Node;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::ai_manager::JavaProfileSource;
use crate::connection_store::{import::normalize_java_group_path, valid_group_path};
use crate::job_executor::virtual_terminal::Options as TerminalOptions;
use crate::scheduler_window::{
    ArchiveFormat, ConnectionGroup, ConnectionRef, ImportedAiAction, JournalDetail,
    MAX_ARCHIVE_EXCLUDES, MAX_ARCHIVE_SOURCES, MAX_SELECTED_GROUPS, MAX_TARGETS, RemoteAction,
    RemotePath, SnippetLink, SshCommand, SudoMode, SymbolicMode, TargetGroups, agent::Kind,
    imported_archive, imported_privileged, imported_transfer, mode_from_text, owner_id_from_text,
};
use crate::session_journal::automation::{Closing, Keep, MAX_DAYS, Options, Retention};
use crate::snippets::SnippetId;

use super::java_secrets::parse_xml;

const MAX_XML_BYTES: usize = 8 * 1024 * 1024;
const MAX_SOURCE_ID_BYTES: usize = 128;
const MAX_ARGUMENTS: usize = 64;
const MAX_TIMES: usize = 24 * 60;
const DEFAULT_JAVA_SWARM_PARALLELISM: i32 = 4;
const ARCHIVE_TARGET_ID_PREFIX: &str = "java-archive-";
const STAGING_TARGET_ID_PREFIX: &str = "java-staging-";
const PRIVILEGED_TARGET_ID_PREFIX: &str = "java-privileged-";

#[derive(Default)]
pub(super) struct Review {
    pub(super) jobs: Vec<ImportedJob>,
    pub(super) omitted: BTreeMap<Omission, usize>,
    pub(super) unsupported_sections: usize,
}

pub(super) struct BoundReview {
    pub(super) jobs: JobGraph<SshCommand>,
    pub(super) omitted: BTreeMap<Omission, usize>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum Omission {
    InvalidDefinition,
    UnsupportedAction,
    UnsupportedExecutionOptions,
    GroupTargets,
    InvalidTargets,
    MissingSnippet,
    InvalidJournalSettings,
    InvalidDependency,
    TargetFanoutLimit,
    InvalidGraph,
}

impl Omission {
    pub(super) fn description(self) -> &'static str {
        match self {
            Self::InvalidDefinition => "invalid Java Scheduler definitions omitted",
            Self::UnsupportedAction => "unsupported Java Scheduler actions omitted",
            Self::UnsupportedExecutionOptions => {
                "Java Scheduler jobs with unsupported execution options omitted"
            }
            Self::GroupTargets => {
                "Java Scheduler jobs with missing or invalid connection groups omitted"
            }
            Self::InvalidTargets => {
                "Java Scheduler jobs with missing or invalid target references omitted"
            }
            Self::MissingSnippet => {
                "Java Scheduler jobs with missing or conflicting snippets omitted"
            }
            Self::InvalidJournalSettings => {
                "Java Scheduler jobs with invalid session journal settings omitted"
            }
            Self::InvalidDependency => "Java Scheduler jobs with missing prerequisites omitted",
            Self::TargetFanoutLimit => {
                "Multi-target Java Scheduler jobs that exceed the Rust job or dependency limits omitted"
            }
            Self::InvalidGraph => "Java Scheduler jobs in an invalid dependency graph omitted",
        }
    }
}

pub(super) struct ImportedJob {
    pub(super) definition: JobDefinition,
    pub(super) source_targets: Vec<String>,
    pub(super) source_groups: Vec<String>,
    pub(super) action: Action,
    pub(super) working_directory: Option<String>,
    virtual_terminal: Option<TerminalOptions>,
    journal_detail: JournalDetail,
    automation_journal: Option<Options>,
}

pub(super) enum Action {
    Command(String),
    Snippet {
        id: SnippetId,
        arguments: Vec<String>,
    },
    Ai(ImportedAiAction),
    Remote(Box<RemoteAction>),
    Privileged(Box<imported_privileged::Settings>),
    PerRoute(RouteDraft),
    Transfer(Box<imported_transfer::Draft>),
}

pub(super) enum RouteDraft {
    Archive(Box<imported_archive::Settings>),
    StagedSftp(Box<imported_transfer::StagingSettings>),
    Privileged(Box<imported_privileged::Settings>),
}

impl RouteDraft {
    fn target_id(&self, job: &JobId, source_id: &str) -> ParseResult<JobId> {
        let (prefix, domain) = match self {
            Self::Archive(_) => (
                ARCHIVE_TARGET_ID_PREFIX,
                "kortty/java-scheduler/archive-target/v1\0",
            ),
            Self::StagedSftp(_) => (
                STAGING_TARGET_ID_PREFIX,
                "kortty/java-scheduler/staging-target/v1\0",
            ),
            Self::Privileged(_) => (
                PRIVILEGED_TARGET_ID_PREFIX,
                "kortty/java-scheduler/privileged-target/v1\0",
            ),
        };
        let mut digest = Sha256::new();
        digest.update(domain);
        digest.update(job.as_str());
        digest.update(b"\0");
        digest.update(source_id);
        let fingerprint = format!("{:x}", digest.finalize());
        JobId::new(format!("{prefix}{}", &fingerprint[..32]))
            .map_err(|_| Omission::InvalidDefinition)
    }
}

impl Action {
    pub(super) fn snippet_id(&self) -> Option<SnippetId> {
        match self {
            Self::Snippet { id, .. } => Some(*id),
            Self::Privileged(settings) => match settings.as_ref() {
                imported_privileged::Settings::Snippet { link, .. } => Some(link.id),
                _ => None,
            },
            _ => None,
        }
    }

    fn target_limit(&self) -> usize {
        match self {
            Self::Ai(draft) if draft.kind == Kind::Swarm => MAX_SWARM_TARGETS,
            _ => MAX_TARGETS,
        }
    }
}

pub(super) type ResolvedGroup = (Vec<ConnectionGroup>, Vec<(String, ConnectionRef)>, bool);

impl ImportedJob {
    #[cfg(test)]
    fn bind(
        self,
        connection: impl FnMut(&str) -> Option<ConnectionRef>,
        mut group: impl FnMut(&str) -> Option<Vec<(String, ConnectionRef)>>,
        rsync_folder: impl FnMut(&str) -> Option<String>,
        has_snippet: impl FnOnce(SnippetId) -> bool,
    ) -> ParseResult<Vec<Job<SshCommand>>> {
        self.bind_scoped(
            connection,
            |path| group(path).map(|members| (vec![path.into()], members, false)),
            rsync_folder,
            has_snippet,
        )
    }

    /// The caller resolves source IDs and complete groups against the reviewed restored routes.
    fn bind_scoped(
        self,
        mut connection: impl FnMut(&str) -> Option<ConnectionRef>,
        mut group: impl FnMut(&str) -> Option<ResolvedGroup>,
        mut rsync_folder: impl FnMut(&str) -> Option<String>,
        has_snippet: impl FnOnce(SnippetId) -> bool,
    ) -> ParseResult<Vec<Job<SshCommand>>> {
        let mut targets = self
            .source_targets
            .into_iter()
            .map(|id| {
                let route = connection(&id).ok_or(Omission::InvalidTargets)?;
                Ok((id, route))
            })
            .collect::<ParseResult<Vec<_>>>()?;
        let mut ids = BTreeSet::new();
        if targets.iter().any(|(_, route)| !ids.insert(route.id)) {
            return Err(Omission::InvalidTargets);
        }
        let target_groups = if self.source_groups.is_empty() {
            None
        } else {
            let explicit: Vec<_> = targets.iter().map(|(_, route)| route.clone()).collect();
            let limit = self.action.target_limit();
            let mut groups = Vec::new();
            let mut preserve_archive_order = false;
            for path in &self.source_groups {
                let (native_groups, members, reordered) = group(path)
                    .filter(|(_, members, _)| !members.is_empty())
                    .ok_or(Omission::GroupTargets)?;
                preserve_archive_order |= reordered;
                for native in native_groups {
                    if !groups.contains(&native) {
                        groups.push(native);
                    }
                }
                for (id, route) in members {
                    if ids.insert(route.id) {
                        if targets.len() == limit {
                            return Err(Omission::InvalidTargets);
                        }
                        targets.push((id, route));
                    }
                }
            }
            let group_order = if preserve_archive_order
                || groups
                    .iter()
                    .any(|group| matches!(group, ConnectionGroup::Teamwork(_)))
            {
                targets[explicit.len()..]
                    .iter()
                    .map(|(_, route)| route.id)
                    .collect()
            } else {
                Vec::new()
            };
            Some(TargetGroups {
                groups,
                explicit,
                group_order,
            })
        };
        if self.virtual_terminal.is_some() && targets.len() > 1 {
            return Err(Omission::UnsupportedExecutionOptions);
        }
        if targets.is_empty() {
            return Err(Omission::InvalidTargets);
        }
        if self.action.snippet_id().is_some_and(|id| !has_snippet(id)) {
            return Err(Omission::MissingSnippet);
        }
        let mut imported_transfer = None;
        let mut imported_privileged = None;
        let mut route_draft = None;
        let (command, snippet, imported_ai, remote) = match self.action {
            Action::Command(command) => (command, None, None, None),
            Action::Snippet { id, arguments } => (
                String::new(),
                Some(SnippetLink { id, arguments }),
                None,
                None,
            ),
            Action::Ai(draft) => (String::new(), None, Some(draft), None),
            Action::Remote(remote) => (String::new(), None, None, Some(*remote)),
            Action::Privileged(settings) => {
                if settings.requires_single_route() {
                    route_draft = Some(RouteDraft::Privileged(settings));
                } else {
                    imported_privileged = Some(Box::new(imported_privileged::Draft {
                        routes: targets.iter().map(|(_, route)| route.clone()).collect(),
                        settings: *settings,
                    }));
                }
                (String::new(), None, None, None)
            }
            Action::PerRoute(draft) => {
                route_draft = Some(draft);
                (String::new(), None, None, None)
            }
            Action::Transfer(mut draft) => {
                if let imported_transfer::Draft::Rsync(rsync) = draft.as_mut()
                    && rsync.direction == imported_transfer::Direction::Download
                    && targets.len() > 1
                {
                    rsync.java_target_folders = targets
                        .iter()
                        .filter_map(|(source, route)| {
                            Some((route.id.to_string(), rsync_folder(source)?))
                        })
                        .collect();
                }
                imported_transfer = Some(draft);
                (String::new(), None, None, None)
            }
        };
        let mut action = SshCommand {
            host: String::new(),
            port: 0,
            username: String::new(),
            key_file: PathBuf::new(),
            connection: None,
            additional_connections: Vec::new(),
            target_groups: None,
            command,
            working_directory: self.working_directory,
            script: None,
            snippet,
            remote,
            agent: None,
            imported_ai,
            imported_transfer,
            imported_privileged,
            sudo: SudoMode::Disabled,
            virtual_terminal: self.virtual_terminal,
            journal_detail: self.journal_detail,
            automation_journal: self.automation_journal,
            desktop_notices: Default::default(),
            webhook_targets: Vec::new(),
        };
        if let Some(draft) = route_draft {
            // Each destination gets its own review and native path.
            let count = targets.len();
            return targets
                .into_iter()
                .enumerate()
                .map(|(index, (id, route))| {
                    let mut definition = self.definition.clone();
                    if count > 1 {
                        definition.id = draft.target_id(&definition.id, &id)?;
                        let suffix = format!(" (target {}/{count})", index + 1);
                        let remaining = kortty_diagram::MAX_LABEL_CHARS - suffix.len();
                        definition.name = definition.name.chars().take(remaining).collect();
                        definition.name.push_str(&suffix);
                    }
                    let mut action = action.clone();
                    action.connection = Some(route.clone());
                    match &draft {
                        RouteDraft::Archive(settings) => {
                            action.remote = Some(RemoteAction::ImportedArchive(Box::new(
                                imported_archive::Draft {
                                    route,
                                    settings: (**settings).clone(),
                                },
                            )));
                        }
                        RouteDraft::StagedSftp(settings) => {
                            action.imported_transfer =
                                Some(Box::new(imported_transfer::Draft::StagedSftp(
                                    imported_transfer::StagedSftpDraft {
                                        route,
                                        settings: (**settings).clone(),
                                    },
                                )));
                        }
                        RouteDraft::Privileged(settings) => {
                            action.imported_privileged =
                                Some(Box::new(imported_privileged::Draft {
                                    routes: vec![route],
                                    settings: (**settings).clone(),
                                }));
                        }
                    }
                    reviewed_job(definition, action)
                })
                .collect();
        }
        let mut routes = targets.into_iter().map(|(_, route)| route);
        action.connection = routes.next();
        action.additional_connections = routes.collect();
        action.target_groups = target_groups;
        reviewed_job(self.definition, action).map(|job| vec![job])
    }
}

fn reviewed_job(definition: JobDefinition, action: SshCommand) -> ParseResult<Job<SshCommand>> {
    let action = action
        .validated()
        .map_err(|_| Omission::InvalidDefinition)?;
    definition
        .with_action(action)
        .map_err(|_| Omission::InvalidDefinition)
}

pub(super) fn inspect(bytes: &[u8]) -> io::Result<Review> {
    let doc = parse_xml(bytes, MAX_XML_BYTES, "jobScheduler")?;
    if doc.root().children().any(|node| node.is_pi()) {
        return Err(invalid("Invalid Java Scheduler processing instruction."));
    }
    let root = doc.root_element();
    let fields = Fields::new(root).map_err(|_| invalid("Invalid Java Scheduler document."))?;
    let mut review = Review::default();
    for section in [
        "sudoCredentials",
        "pinnedHostKeys",
        "journal",
        "webhookTargets",
    ] {
        if fields
            .get(section)
            .is_some_and(|node| node.children().any(|child| child.is_element()))
        {
            review.unsupported_sections += 1;
        }
    }
    let jobs = fields
        .list("jobs", "job", MAX_JOBS)
        .map_err(|_| invalid("Invalid Java Scheduler list or job limit exceeded."))?;
    let mut ids = BTreeSet::new();
    for job in jobs {
        let Ok(fields) = Fields::new(job) else {
            *review
                .omitted
                .entry(Omission::InvalidDefinition)
                .or_default() += 1;
            continue;
        };
        let id = fields.text("id").and_then(job_id);
        if let Ok(ref id) = id
            && !ids.insert(id.clone())
        {
            return Err(invalid("Duplicate Java Scheduler job identity."));
        }
        match id.and_then(|id| parse_job(&fields, id)) {
            Ok(job) => review.jobs.push(job),
            Err(reason) => *review.omitted.entry(reason).or_default() += 1,
        }
    }
    let missing = prune_missing_dependencies(
        &mut review.jobs,
        |job| &job.definition.id,
        |job| &job.definition.dependencies,
    );
    if missing != 0 {
        *review
            .omitted
            .entry(Omission::InvalidDependency)
            .or_default() += missing;
    }
    let graph = review
        .jobs
        .iter()
        .map(|job| job.definition.clone().with_action(()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(io::Error::other)?;
    if JobGraph::new(graph).is_err() {
        *review.omitted.entry(Omission::InvalidGraph).or_default() += review.jobs.len();
        review.jobs.clear();
    }
    Ok(review)
}

impl Review {
    pub(super) fn associate_ai_profiles<'a>(
        &mut self,
        sources: impl Iterator<Item = &'a JavaProfileSource>,
    ) {
        let sources: BTreeMap<_, _> = sources.map(|source| (source.id(), source)).collect();
        let matching_source =
            |id: Option<&str>| id.and_then(|id| sources.get(id).copied()).cloned();
        for job in &mut self.jobs {
            if let Some(options) = &mut job.automation_journal {
                options.java_profile_source =
                    matching_source(options.unresolved_summary_profile.as_deref());
            }
            if let Action::Ai(draft) = &mut job.action {
                draft.profile_source = matching_source(draft.profile_id.as_deref());
            }
        }
    }

    pub(super) fn ai_drafts(&self) -> usize {
        self.jobs
            .iter()
            .filter(|job| matches!(job.action, Action::Ai(_)))
            .count()
    }

    pub(super) fn unconfirmed_deletions(&self) -> usize {
        self.jobs
            .iter()
            .filter(|job| matches!(&job.action, Action::Remote(remote) if matches!(**remote, RemoteAction::Delete { .. })))
            .count()
    }

    pub(super) fn remote_copies(&self) -> usize {
        self.jobs
            .iter()
            .filter(|job| matches!(&job.action, Action::Remote(remote) if matches!(**remote, RemoteAction::CopyRemote { .. })))
            .count()
    }

    pub(super) fn archive_drafts(&self) -> usize {
        self.jobs
            .iter()
            .filter(|job| matches!(job.action, Action::PerRoute(RouteDraft::Archive(_))))
            .count()
    }

    pub(super) fn archive_passwords_to_select(&self) -> usize {
        self.jobs.iter().filter(|job| {
            matches!(&job.action, Action::PerRoute(RouteDraft::Archive(settings)) if settings.format == ArchiveFormat::ZipPassword)
        }).count()
    }

    pub(super) fn privileged_archives_to_review(&self) -> usize {
        self.jobs
            .iter()
            .filter(|job| matches!(&job.action, Action::PerRoute(RouteDraft::Archive(settings)) if settings.java_sudo))
            .count()
    }

    pub(super) fn archive_fanouts(&self) -> usize {
        self.jobs
            .iter()
            .filter(|job| {
                matches!(job.action, Action::PerRoute(RouteDraft::Archive(_)))
                    && (job.source_targets.len() > 1 || !job.source_groups.is_empty())
            })
            .count()
    }

    pub(super) fn unresolved_journal_profiles(&self) -> usize {
        self.jobs
            .iter()
            .filter(|job| {
                job.automation_journal
                    .as_ref()
                    .is_some_and(|options| options.unresolved_summary_profile.is_some())
            })
            .count()
    }

    pub(super) fn transfer_drafts(&self) -> usize {
        self.jobs
            .iter()
            .filter(|job| {
                matches!(&job.action, Action::Transfer(draft)
                    if matches!(draft.as_ref(), imported_transfer::Draft::Sftp(_)))
            })
            .count()
    }

    pub(super) fn rsync_drafts(&self) -> usize {
        self.jobs
            .iter()
            .filter(|job| {
                matches!(&job.action, Action::Transfer(draft)
                    if matches!(draft.as_ref(), imported_transfer::Draft::Rsync(_)))
            })
            .count()
    }

    pub(super) fn staging_drafts(&self) -> usize {
        self.jobs
            .iter()
            .filter(|job| matches!(job.action, Action::PerRoute(RouteDraft::StagedSftp(_))))
            .count()
    }

    pub(super) fn privileged_drafts(&self) -> usize {
        self.jobs
            .iter()
            .filter(|job| matches!(job.action, Action::Privileged(_)))
            .count()
    }

    #[cfg(test)]
    pub(super) fn bind_all(
        self,
        connection: impl FnMut(&str) -> Option<ConnectionRef>,
        mut group: impl FnMut(&str) -> Option<Vec<(String, ConnectionRef)>>,
        rsync_folder: impl FnMut(&str) -> Option<String>,
        has_snippet: impl FnMut(SnippetId) -> bool,
    ) -> io::Result<BoundReview> {
        self.bind_all_scoped(
            connection,
            |path| group(path).map(|members| (vec![path.into()], members, false)),
            rsync_folder,
            has_snippet,
        )
    }

    pub(super) fn bind_all_scoped(
        self,
        mut connection: impl FnMut(&str) -> Option<ConnectionRef>,
        mut group: impl FnMut(&str) -> Option<ResolvedGroup>,
        mut rsync_folder: impl FnMut(&str) -> Option<String>,
        mut has_snippet: impl FnMut(SnippetId) -> bool,
    ) -> io::Result<BoundReview> {
        let mut source_jobs = Vec::with_capacity(self.jobs.len());
        let mut omitted = self.omitted;
        for incoming in self.jobs {
            let source = incoming.definition.id.clone();
            match incoming.bind_scoped(
                &mut connection,
                &mut group,
                &mut rsync_folder,
                &mut has_snippet,
            ) {
                Ok(jobs) => source_jobs.push((source, jobs)),
                Err(reason) => *omitted.entry(reason).or_default() += 1,
            }
        }
        let targets = loop {
            let missing = prune_missing_dependencies(
                &mut source_jobs,
                |(id, _)| id,
                |(_, jobs)| jobs[0].dependencies(),
            );
            if missing != 0 {
                *omitted.entry(Omission::InvalidDependency).or_default() += missing;
            }
            let targets: BTreeMap<_, Vec<_>> = source_jobs
                .iter()
                .map(|(id, jobs)| (id.clone(), jobs.iter().map(Job::id).cloned().collect()))
                .collect();
            let job_count: usize = source_jobs.iter().map(|(_, jobs)| jobs.len()).sum();
            let edge_count: usize = source_jobs
                .iter()
                .map(|(_, jobs)| {
                    jobs.len()
                        * jobs[0]
                            .dependencies()
                            .iter()
                            .map(|id| targets[id].len())
                            .sum::<usize>()
                })
                .sum();
            if job_count <= MAX_JOBS && edge_count <= MAX_DEPENDENCIES {
                break targets;
            }
            let Some(index) = source_jobs.iter().rposition(|(_, jobs)| jobs.len() > 1) else {
                return Err(invalid("Invalid reviewed Java Scheduler graph."));
            };
            source_jobs.remove(index);
            *omitted.entry(Omission::TargetFanoutLimit).or_default() += 1;
        };
        let mut jobs = Vec::new();
        for (_, bound) in source_jobs {
            for job in bound {
                // A dependent must wait for every target of a split Java job.
                let dependencies = job
                    .dependencies()
                    .iter()
                    .flat_map(|id| targets[id].iter().cloned())
                    .collect();
                let mode = job.mode();
                let max_age = job.max_age_minutes();
                jobs.push(job.with_dependencies(mode, dependencies, max_age));
            }
        }
        Ok(BoundReview {
            jobs: JobGraph::new(jobs).map_err(io::Error::other)?,
            omitted,
        })
    }
}

fn prune_missing_dependencies<T>(
    jobs: &mut Vec<T>,
    id: impl Fn(&T) -> &JobId,
    dependencies: impl Fn(&T) -> &[JobId],
) -> usize {
    let mut ids: BTreeSet<_> = jobs.iter().map(|job| id(job).clone()).collect();
    let original = jobs.len();
    loop {
        let before = ids.len();
        for job in jobs.iter() {
            if dependencies(job)
                .iter()
                .any(|dependency| !ids.contains(dependency))
            {
                ids.remove(id(job));
            }
        }
        if ids.len() == before {
            jobs.retain(|job| ids.contains(id(job)));
            return original - jobs.len();
        }
    }
}

/// Encodes only reviewed disabled jobs, with native portable Scheduler JSON.
pub(super) fn encode(graph: &JobGraph<SshCommand>) -> io::Result<Zeroizing<Vec<u8>>> {
    check_bound_graph(graph)?;
    SchedulerStore::encode_backup(graph)
        .map(Zeroizing::new)
        .map_err(io::Error::other)
}

/// Revalidates the exact allowed job/action shape before applying a pending restore.
pub(super) fn decode(bytes: &[u8]) -> io::Result<JobGraph<SshCommand>> {
    let graph = SchedulerStore::decode_backup(bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    check_bound_graph(&graph)?;
    Ok(graph)
}

fn check_bound_graph(graph: &JobGraph<SshCommand>) -> io::Result<()> {
    if graph.jobs().is_empty() || !graph.jobs().iter().all(valid_bound_job) {
        return Err(invalid("Invalid reviewed Java Scheduler jobs."));
    }
    Ok(())
}

fn valid_bound_job(job: &Job<SshCommand>) -> bool {
    let action = job.action();
    !job.enabled()
        && valid_bound_id(job)
        && action.connection.is_some()
        && action.script.is_none()
        && action.agent.is_none()
        && action.sudo == SudoMode::Disabled
        && action
            .automation_journal
            .as_ref()
            .is_none_or(|options| options.summary_profile.is_none())
        && action.desktop_notices == Default::default()
        && action.webhook_targets.is_empty()
        && action
            .working_directory
            .as_deref()
            .is_none_or(|value| bounded(value, 4096, false).is_ok())
        && match (
            &action.snippet,
            &action.imported_ai,
            &action.remote,
            &action.imported_transfer,
            &action.imported_privileged,
        ) {
            (Some(link), None, None, None, None) => {
                action.command.is_empty()
                    && link
                        .arguments
                        .iter()
                        .all(|value| bounded(value, 4096, false).is_ok())
            }
            (None, Some(_), None, None, None)
            | (None, None, None, Some(_), None)
            | (None, None, None, None, Some(_)) => action.command.is_empty(),
            (None, None, Some(remote), None, None) => {
                action.command.is_empty() && valid_remote_import(remote)
            }
            (None, None, None, None, None) => bounded(&action.command, 16 * 1024, true).is_ok(),
            _ => false,
        }
        && action.clone().validated().is_ok()
}

fn valid_bound_id(job: &Job<SshCommand>) -> bool {
    let id = job.id().as_str();
    let suffix = if let Some(suffix) = id.strip_prefix(ARCHIVE_TARGET_ID_PREFIX) {
        matches!(job.action().remote, Some(RemoteAction::ImportedArchive(_))).then_some(suffix)
    } else if let Some(suffix) = id.strip_prefix(STAGING_TARGET_ID_PREFIX) {
        matches!(
            job.action().imported_transfer.as_deref(),
            Some(imported_transfer::Draft::StagedSftp(_))
        )
        .then_some(suffix)
    } else if let Some(suffix) = id.strip_prefix(PRIVILEGED_TARGET_ID_PREFIX) {
        job.action()
            .imported_privileged
            .as_ref()
            .is_some_and(|draft| draft.settings.requires_single_route())
            .then_some(suffix)
    } else {
        id.strip_prefix("java-")
    };
    suffix.is_some_and(|value| {
        value.len() == 32
            && value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

fn valid_remote_import(remote: &RemoteAction) -> bool {
    let paths = match remote {
        RemoteAction::Mkdir { path } if valid_mkdir_path(path) => (path, None),
        RemoteAction::Chmod { path, .. }
        | RemoteAction::ChmodSymbolic { path, .. }
        | RemoteAction::Chown {
            path, uid: Some(_), ..
        } => (path, None),
        RemoteAction::Delete {
            path,
            confirmed_path,
            recursive: true,
        } if confirmed_path.is_empty() => (path, None),
        RemoteAction::Rename { from, to } | RemoteAction::CopyRemote { from, to } => {
            (from, Some(to))
        }
        RemoteAction::ImportedArchive(draft) => return draft.settings.validate().is_ok(),
        _ => return false,
    };
    std::iter::once(paths.0)
        .chain(paths.1)
        .all(|path| bounded(path.as_str(), 4096, false).is_ok())
}

fn valid_mkdir_path(path: &RemotePath) -> bool {
    // Java skips blank path components for recursive directory creation.
    path.as_str()[1..]
        .split('/')
        .all(|part| !part.trim().is_empty())
}

type ParseResult<T> = Result<T, Omission>;

fn parse_job(fields: &Fields<'_, '_>, id: JobId) -> ParseResult<ImportedJob> {
    if fields.boolean("hostKeyVerificationDisabled", false)? {
        return Err(Omission::UnsupportedExecutionOptions);
    }
    let action_fields = Fields::new(fields.get("action").ok_or(Omission::InvalidDefinition)?)?;
    let mut action = parse_action(&action_fields)?;
    let file_or_non_shell = matches!(
        action,
        Action::Ai(_) | Action::Remote(_) | Action::Transfer(_) | Action::PerRoute(_)
    ) || matches!(&action, Action::Privileged(settings) if settings.shell_options().is_none());
    let source_targets = parse_targets(fields, action.target_limit())?;
    let source_groups = parse_groups(fields)?;
    if source_targets.is_empty() && source_groups.is_empty() {
        return Err(Omission::InvalidTargets);
    }
    let mut virtual_terminal = if action_fields.boolean("ptyEnabled", false)? {
        if file_or_non_shell
            || (source_targets.len() > 1 && !matches!(action, Action::Privileged(_)))
        {
            return Err(Omission::UnsupportedExecutionOptions);
        }
        Some(parse_virtual_terminal(&action_fields)?)
    } else {
        None
    };
    let journal_detail = if file_or_non_shell {
        JournalDetail::Off
    } else {
        match fields.optional_text("journalDetailMode")? {
            None | Some("LIMITED_REDACTED") => JournalDetail::LimitedRedacted,
            Some("FULL") => JournalDetail::Full,
            _ => return Err(Omission::UnsupportedExecutionOptions),
        }
    };

    let name = fields.text("name")?;
    if !name
        .chars()
        .all(crate::session_journal::is_display_character)
        || Job::new(id.clone(), name, ()).is_err()
    {
        return Err(Omission::InvalidDefinition);
    }
    let mut dependencies = fields
        .text_list("dependsOn", "jobId", MAX_DEPENDENCIES)?
        .iter()
        .map(|id| job_id(id))
        .collect::<ParseResult<Vec<_>>>()?;
    let mut unique = BTreeSet::new();
    dependencies.retain(|value| value != &id && unique.insert(value.clone()));
    let mode = match fields.optional_text("dependencyMode")? {
        None | Some("GATE") => DependencyMode::Gate,
        Some("CHAIN") => DependencyMode::Chain,
        _ => return Err(Omission::InvalidDependency),
    };
    let definition = JobDefinition {
        id,
        name: name.into(),
        enabled: false,
        mode,
        max_age_minutes: fields.positive_minutes("dependencyMaxAgeMinutes")?,
        dependencies,
        schedule: fields.get("schedule").map(parse_schedule).transpose()?,
    };
    let mut working_directory = None;
    // Java consults this field when constructing commands and AI requests.
    if !matches!(
        action,
        Action::Remote(_) | Action::Transfer(_) | Action::PerRoute(_)
    ) && !matches!(&action, Action::Privileged(settings) if settings.shell_options().is_none())
    {
        working_directory = fields
            .optional_text("workingDirectory")?
            .map(|path| bounded(path, 4096, false).map(str::to_owned))
            .transpose()?;
        if let Action::Ai(draft) = &mut action {
            draft.working_directory = working_directory.take();
        }
    }
    if let Action::Privileged(settings) = &mut action {
        match settings.as_mut() {
            imported_privileged::Settings::Command { options, .. }
            | imported_privileged::Settings::Snippet { options, .. } => {
                options.working_directory = working_directory.take();
                options.virtual_terminal = virtual_terminal.take();
            }
            imported_privileged::Settings::File { .. } => {}
        }
    }
    let automation_journal = fields
        .get("sessionJournal")
        .map(parse_session_journal)
        .transpose()
        .map_err(|_| Omission::InvalidJournalSettings)?;
    Ok(ImportedJob {
        definition,
        source_targets,
        source_groups,
        action,
        working_directory,
        virtual_terminal,
        journal_detail,
        automation_journal,
    })
}

fn parse_targets(fields: &Fields<'_, '_>, limit: usize) -> ParseResult<Vec<String>> {
    let mut ids = Vec::new();
    let mut seen = BTreeSet::new();
    for node in fields.list("targetConnectionIds", "connectionId", limit)? {
        let id = trim_java_list_item(element_text(node)?);
        if !id.is_empty() && seen.insert(id) {
            ids.push(id);
        }
    }
    if ids.is_empty()
        && let Some(node) = fields.get("connectionId")
    {
        // JAXB reads the legacy singular ID directly. Java resolves it as stored.
        let id = element_text(node)?;
        if !trim_java_list_item(id).is_empty() {
            ids.push(id);
        }
    }
    if ids.iter().any(|id| !source_id(id)) {
        return Err(Omission::InvalidTargets);
    }
    Ok(ids.into_iter().map(str::to_owned).collect())
}

fn parse_groups(fields: &Fields<'_, '_>) -> ParseResult<Vec<String>> {
    let mut groups = Vec::new();
    for node in fields.list("targetGroupNames", "groupName", MAX_SELECTED_GROUPS)? {
        let raw = trim_java_list_item(element_text(node)?);
        if raw.is_empty() {
            continue;
        }
        let path = normalize_java_group_path(raw);
        // A slash-only Java selection means the root, which Rust cannot store as a selected group.
        if !valid_group_path(&path) {
            return Err(Omission::GroupTargets);
        }
        if !groups.contains(&path) {
            groups.push(path);
        }
    }
    Ok(groups)
}

fn trim_java_list_item(value: &str) -> &str {
    value.trim_matches(|character| character <= '\u{20}')
}

fn parse_virtual_terminal(fields: &Fields<'_, '_>) -> ParseResult<TerminalOptions> {
    let defaults = TerminalOptions::default();
    Ok(TerminalOptions {
        columns: fields
            .integer("ptyColumns", i32::from(defaults.columns))?
            .clamp(40, 400) as u16,
        rows: fields
            .integer("ptyRows", i32::from(defaults.rows))?
            .clamp(10, 200) as u16,
        interval_seconds: fields
            .integer(
                "screenshotIntervalSeconds",
                i32::from(defaults.interval_seconds),
            )?
            .clamp(0, 3600) as u16,
        capture_changes: fields.boolean("screenshotOnChange", defaults.capture_changes)?,
        max_frames: fields
            .integer("maxScreenshots", i32::from(defaults.max_frames))?
            .clamp(1, 500) as u16,
        limit_seconds: fields
            .integer("runtimeLimitSeconds", defaults.limit_seconds as i32)?
            .clamp(0, 86_400) as u32,
        limit_is_success: fields.boolean("runtimeLimitSuccess", defaults.limit_is_success)?,
    })
}

pub(crate) fn read_swarm_journal(node: Node<'_, '_>) -> Option<Options> {
    parse_session_journal(node).ok()
}

fn parse_session_journal(node: Node<'_, '_>) -> ParseResult<Options> {
    let fields = Fields::new(node)?;
    let days = fields
        .integer("retentionDays", 14)?
        .clamp(1, i32::from(MAX_DAYS)) as u16;
    let retention = match fields.optional_text("retentionMode")? {
        None | Some("DAYS") => Retention::Days { days },
        Some("FIXED_DATE") => match fields.optional_text("expiryDate")? {
            Some(date) if parse_date(date).is_ok() => Retention::Date { date: date.into() },
            _ => Retention::Days { days },
        },
        Some("NONE") => Retention::Forever,
        _ => return Err(Omission::InvalidDefinition),
    };
    let keep = match fields.optional_text("keepMode")? {
        None | Some("ALWAYS") => Keep::Always,
        Some("ONLY_ON_FAILURE") => Keep::Failures,
        _ => return Err(Omission::InvalidDefinition),
    };
    let closing = match fields.optional_text("aiMode")? {
        None | Some("ALWAYS") => Closing::Always,
        Some("ON_FAILURE") => Closing::Failures,
        Some("OFF") => Closing::Off,
        _ => return Err(Omission::InvalidDefinition),
    };
    let unresolved_summary_profile = fields
        .optional_text("aiProfileId")?
        .map(|value| {
            JavaProfileSource::parse_id(value)
                .map(str::to_owned)
                .ok_or(Omission::InvalidDefinition)
        })
        .transpose()?;
    let options = Options {
        enabled: fields.boolean("enabled", false)?,
        capture_input: fields.boolean("captureInput", true)?,
        keep,
        closing,
        summary_profile: None,
        unresolved_summary_profile,
        java_profile_source: None,
        retention,
        max_runs: u16::try_from(fields.integer("maxJournals", 0)?.max(0))
            .map_err(|_| Omission::InvalidDefinition)?,
        max_storage_mib: fields.integer("maxStorageMb", 0)?.max(0) as u32,
        deduplicate: fields.boolean("dedupEnabled", true)?,
    };
    options
        .validate()
        .map_err(|_| Omission::InvalidDefinition)?;
    Ok(options)
}

fn parse_schedule(node: Node<'_, '_>) -> ParseResult<JobSchedule> {
    let fields = Fields::new(node)?;
    let weekdays = fields
        .text_list("weekdays", "weekday", 7)?
        .iter()
        .map(|day| match day.to_ascii_uppercase().as_str() {
            "MONDAY" => Ok(Weekday::Monday),
            "TUESDAY" => Ok(Weekday::Tuesday),
            "WEDNESDAY" => Ok(Weekday::Wednesday),
            "THURSDAY" => Ok(Weekday::Thursday),
            "FRIDAY" => Ok(Weekday::Friday),
            "SATURDAY" => Ok(Weekday::Saturday),
            "SUNDAY" => Ok(Weekday::Sunday),
            _ => Err(Omission::InvalidDefinition),
        })
        .collect::<ParseResult<Vec<_>>>()?;
    let times = fields
        .text_list("fixedTimes", "time", MAX_TIMES)?
        .iter()
        .map(|time| parse_time(time))
        .collect::<ParseResult<Vec<_>>>()?;
    let start = fields
        .optional_text("windowStartTime")?
        .map(parse_time)
        .transpose()?
        .unwrap_or(Time::MIN);
    let end = fields
        .optional_text("windowEndTime")?
        .map(parse_time)
        .transpose()?
        .unwrap_or(Time::new(23, 59, 0, 0).expect("valid day end"));
    let from = fields
        .optional_text("activeFromDate")?
        .map(parse_date)
        .transpose()?;
    let until = fields
        .optional_text("activeUntilDate")?
        .map(parse_date)
        .transpose()?;
    JobSchedule::default()
        .with_enabled(fields.boolean("enabled", true)?)
        .with_weekdays(weekdays)
        .with_fixed_times(times)
        .with_window(start, end)
        .with_interval_minutes(fields.positive_minutes("intervalMinutes")?)
        .with_active_dates(from, until)
        .map_err(|_| Omission::InvalidDefinition)
}

fn parse_action(fields: &Fields<'_, '_>) -> ParseResult<Action> {
    let kind = fields.optional_text("type")?.unwrap_or("COMMAND");
    if !matches!(
        kind,
        "COMMAND"
            | "SNIPPET_SCRIPT"
            | "AI_AGENT"
            | "AI_SWARM"
            | "SFTP_MKDIR"
            | "SFTP_RENAME"
            | "SFTP_CHMOD"
            | "SFTP_CHOWN"
            | "SFTP_COPY_REMOTE"
            | "SFTP_DELETE"
            | "SFTP_ARCHIVE"
            | "SFTP_UPLOAD"
            | "SFTP_DOWNLOAD"
            | "SFTP_SYNC"
            | "RSYNC_SYNC"
    ) {
        return Err(Omission::UnsupportedAction);
    }
    let use_sudo = fields.boolean("useSudo", false)?;
    let sudo_staging = fields.boolean("sudoStagingEnabled", false)?;
    if !matches!(
        kind,
        "SFTP_ARCHIVE" | "RSYNC_SYNC" | "SFTP_UPLOAD" | "SFTP_DOWNLOAD" | "SFTP_SYNC"
    ) && (sudo_staging || (use_sudo && matches!(kind, "AI_AGENT" | "AI_SWARM")))
    {
        return Err(Omission::UnsupportedExecutionOptions);
    }
    match kind {
        "COMMAND" => {
            let text = bounded(fields.text("command")?, 16 * 1024, true)?.to_owned();
            Ok(if use_sudo {
                Action::Privileged(Box::new(imported_privileged::Settings::Command {
                    text,
                    options: Default::default(),
                }))
            } else {
                Action::Command(text)
            })
        }
        "SNIPPET_SCRIPT" => {
            let id = fields.text("snippetId")?;
            let compact = compact_uuid(id).ok_or(Omission::InvalidDefinition)?;
            let id = SnippetId::try_from(compact).map_err(|_| Omission::InvalidDefinition)?;
            let arguments = fields
                .list("snippetArguments", "argument", MAX_ARGUMENTS)?
                .into_iter()
                .map(element_text)
                .collect::<ParseResult<Vec<_>>>()?
                .into_iter()
                .filter(|value| !value.trim().is_empty())
                .map(|value| bounded(value, 4096, false).map(str::to_owned))
                .collect::<ParseResult<Vec<_>>>()?;
            if arguments
                .iter()
                .map(|value| value.len() + 1)
                .sum::<usize>()
                .saturating_sub(1)
                > 8 * 1024
            {
                return Err(Omission::InvalidDefinition);
            }
            Ok(if use_sudo {
                Action::Privileged(Box::new(imported_privileged::Settings::Snippet {
                    link: SnippetLink { id, arguments },
                    options: Default::default(),
                }))
            } else {
                Action::Snippet { id, arguments }
            })
        }
        "AI_AGENT" => parse_ai_action(fields, Kind::Agent),
        "AI_SWARM" => parse_ai_action(fields, Kind::Swarm),
        "SFTP_ARCHIVE" => parse_archive_draft(fields, use_sudo),
        "SFTP_UPLOAD" | "SFTP_DOWNLOAD" | "SFTP_SYNC" => {
            parse_transfer_draft(fields, kind, use_sudo && sudo_staging)
        }
        "RSYNC_SYNC" => parse_rsync_draft(fields, use_sudo),
        _ if use_sudo => parse_privileged_file(fields, kind),
        _ => parse_remote_action(fields, kind).map(|action| Action::Remote(Box::new(action))),
    }
}

fn parse_privileged_file(fields: &Fields<'_, '_>, kind: &str) -> ParseResult<Action> {
    use imported_privileged::FileAction;

    let hint = |field| {
        fields
            .java_text(field)?
            .map(|text| bounded(text, 4096, false).map(str::to_owned))
            .transpose()
    };
    let required = |field| hint(field)?.ok_or(Omission::InvalidDefinition);
    let action = match kind {
        "SFTP_MKDIR" => FileAction::Mkdir {
            path: required("remotePath")?,
        },
        "SFTP_RENAME" => FileAction::Rename {
            from: required("remoteSourcePath")?,
            to: match hint("remoteDestinationPath")? {
                Some(path) => path,
                None => required("newName")?,
            },
        },
        "SFTP_CHMOD" => FileAction::Chmod {
            path: required("remotePath")?,
            permissions: required("permissions")?,
        },
        "SFTP_CHOWN" => FileAction::Chown {
            path: required("remotePath")?,
            owner: required("owner")?,
            group: hint("group")?,
        },
        "SFTP_COPY_REMOTE" => FileAction::Copy {
            from: required("remoteSourcePath")?,
            to: required("remoteDestinationPath")?,
        },
        "SFTP_DELETE" => FileAction::Delete {
            path: required("remotePath")?,
        },
        _ => return Err(Omission::UnsupportedAction),
    };
    Ok(Action::Privileged(Box::new(
        imported_privileged::Settings::File { action },
    )))
}

fn parse_archive_draft(fields: &Fields<'_, '_>, use_sudo: bool) -> ParseResult<Action> {
    let format = match fields.java_text("archiveFormat")? {
        None | Some("ZIP") => ArchiveFormat::Zip,
        Some("ZIP_PASSWORD") => ArchiveFormat::ZipPassword,
        Some("TAR") => ArchiveFormat::Tar,
        Some("TAR_BZ2") => ArchiveFormat::TarBz2,
        _ => return Err(Omission::UnsupportedExecutionOptions),
    };
    if format == ArchiveFormat::ZipPassword
        && fields.java_text("encryptedArchivePassword")?.is_none()
    {
        return Err(Omission::InvalidDefinition);
    }
    let level = fields.integer("archiveCompressionLevel", 6)?.clamp(0, 9) as u8;
    if !format.valid_level(level) {
        return Err(Omission::UnsupportedExecutionOptions);
    }
    let list = |name, item, limit| {
        let mut values = Vec::new();
        for node in fields.list(name, item, limit)? {
            let value = trim_java_list_item(element_text(node)?);
            if !value.is_empty() {
                values.push(value);
            }
        }
        Ok::<_, Omission>(values)
    };
    let local_hint = if fields.boolean("archiveDownloadAfterCreate", false)? {
        Some(
            fields
                .java_text("archiveDownloadLocalPath")?
                .ok_or(Omission::InvalidDefinition)?
                .to_owned(),
        )
    } else {
        None
    };
    let settings = imported_archive::Settings {
        sources: list("archiveSources", "path", MAX_ARCHIVE_SOURCES)?
            .into_iter()
            .map(remote_path)
            .collect::<ParseResult<Vec<_>>>()?,
        target: remote_path(
            fields
                .java_text("archivePath")?
                .ok_or(Omission::InvalidDefinition)?,
        )?,
        format,
        level: if format == ArchiveFormat::Tar {
            6
        } else {
            level
        },
        java_excludes: list("archiveExcludes", "pattern", MAX_ARCHIVE_EXCLUDES)?
            .into_iter()
            .map(str::to_owned)
            .collect(),
        java_sudo: use_sudo,
        local_hint,
    };
    settings
        .validate()
        .map_err(|_| Omission::InvalidDefinition)?;
    Ok(Action::PerRoute(RouteDraft::Archive(Box::new(settings))))
}

fn parse_transfer_draft(fields: &Fields<'_, '_>, kind: &str, staged: bool) -> ParseResult<Action> {
    use imported_transfer::Kind;

    let kind = match kind {
        "SFTP_UPLOAD" => Kind::Upload,
        "SFTP_DOWNLOAD" => Kind::Download,
        "SFTP_SYNC" => match fields.java_text("syncDirection")? {
            None | Some("UPLOAD") => Kind::SyncUpload,
            Some("DOWNLOAD") => Kind::SyncDownload,
            _ => return Err(Omission::UnsupportedExecutionOptions),
        },
        _ => return Err(Omission::UnsupportedAction),
    };
    let path = |name| fields.java_text(name)?.ok_or(Omission::InvalidDefinition);
    let local_hint = bounded(path("localPath")?, 4096, false)?.to_owned();
    let remote = path("remotePath")?;
    if staged {
        let settings = imported_transfer::StagingSettings {
            kind,
            local_hint,
            remote_hint: bounded(remote, 4096, false)?.to_owned(),
        };
        settings
            .validate()
            .map_err(|_| Omission::InvalidDefinition)?;
        return Ok(Action::PerRoute(RouteDraft::StagedSftp(Box::new(settings))));
    }
    let remote = remote_path(remote)?;
    Ok(Action::Transfer(Box::new(imported_transfer::Draft::Sftp(
        imported_transfer::SftpDraft {
            kind,
            local_hint,
            remote,
        },
    ))))
}

fn parse_rsync_draft(fields: &Fields<'_, '_>, use_sudo: bool) -> ParseResult<Action> {
    use imported_transfer::{Direction, MAX_RSYNC_SOURCES, RsyncDraft};

    let direction = match fields.java_text("rsyncDirection")? {
        None | Some("UPLOAD") => Direction::Upload,
        Some("DOWNLOAD") => Direction::Download,
        _ => return Err(Omission::UnsupportedExecutionOptions),
    };
    let mut sources = Vec::new();
    for node in fields.list("rsyncSources", "path", MAX_RSYNC_SOURCES)? {
        let source = trim_java_list_item(element_text(node)?);
        if !source.is_empty() {
            sources.push(source.to_owned());
        }
    }
    let draft = RsyncDraft {
        direction,
        java_sources: sources,
        java_target_root: fields
            .java_text("rsyncTargetRoot")?
            .ok_or(Omission::InvalidDefinition)?
            .to_owned(),
        java_delete: fields.boolean("rsyncDeleteEnabled", false)?,
        java_sudo: use_sudo,
        java_target_folders: BTreeMap::new(),
    };
    draft.validate().map_err(|_| Omission::InvalidDefinition)?;
    Ok(Action::Transfer(Box::new(imported_transfer::Draft::Rsync(
        draft,
    ))))
}

fn parse_remote_action(fields: &Fields<'_, '_>, kind: &str) -> ParseResult<RemoteAction> {
    let path = |name| remote_path(fields.java_text(name)?.ok_or(Omission::InvalidDefinition)?);
    let remote = match kind {
        "SFTP_MKDIR" => {
            let path = path("remotePath")?;
            if !valid_mkdir_path(&path) {
                return Err(Omission::InvalidDefinition);
            }
            RemoteAction::Mkdir { path }
        }
        "SFTP_RENAME" => RemoteAction::Rename {
            from: path("remoteSourcePath")?,
            to: match fields.java_text("remoteDestinationPath")? {
                Some(destination) => remote_path(destination)?,
                None => path("newName")?,
            },
        },
        "SFTP_CHMOD" => {
            let path = path("remotePath")?;
            let permissions = fields
                .java_text("permissions")?
                .ok_or(Omission::InvalidDefinition)?;
            if !permissions.is_ascii() {
                return Err(Omission::UnsupportedExecutionOptions);
            }
            match mode_from_text(permissions) {
                Ok(mode) => RemoteAction::Chmod { path, mode },
                Err(_) => RemoteAction::ChmodSymbolic {
                    path,
                    mode: SymbolicMode::try_from(permissions.to_owned())
                        .map_err(|_| Omission::UnsupportedExecutionOptions)?,
                },
            }
        }
        "SFTP_CHOWN" => RemoteAction::Chown {
            path: path("remotePath")?,
            uid: numeric_owner(Some(
                fields
                    .java_text("owner")?
                    .ok_or(Omission::InvalidDefinition)?,
            ))?,
            gid: numeric_owner(fields.java_text("group")?)?,
        },
        "SFTP_COPY_REMOTE" => RemoteAction::CopyRemote {
            from: path("remoteSourcePath")?,
            to: path("remoteDestinationPath")?,
        },
        "SFTP_DELETE" => RemoteAction::Delete {
            path: path("remotePath")?,
            confirmed_path: String::new(),
            recursive: true,
        },
        _ => return Err(Omission::UnsupportedAction),
    };
    remote
        .validate_definition()
        .map_err(|_| Omission::InvalidDefinition)?;
    Ok(remote)
}

fn remote_path(text: &str) -> ParseResult<RemotePath> {
    RemotePath::try_from(bounded(text, 4096, false)?.to_owned())
        .map_err(|_| Omission::InvalidDefinition)
}

fn numeric_owner(value: Option<&str>) -> ParseResult<Option<u32>> {
    let value = value.unwrap_or_default();
    if !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Omission::UnsupportedExecutionOptions);
    }
    owner_id_from_text(value).map_err(|_| Omission::UnsupportedExecutionOptions)
}

fn parse_ai_action(fields: &Fields<'_, '_>, kind: Kind) -> ParseResult<Action> {
    let goal = bounded(fields.text("aiPrompt")?, 16 * 1024, true)?.to_owned();
    let profile_id = fields
        .optional_text("aiProfileId")?
        .map(|value| {
            JavaProfileSource::parse_id(value)
                .map(str::to_owned)
                .ok_or(Omission::InvalidDefinition)
        })
        .transpose()?;
    let (swarm_read_only, parallelism) = if kind == Kind::Swarm {
        let parallelism = fields
            .optional_text("swarmMaxParallelism")?
            .map(str::parse::<i32>)
            .transpose()
            .map_err(|_| Omission::InvalidDefinition)?
            .unwrap_or(DEFAULT_JAVA_SWARM_PARALLELISM);
        (
            fields.boolean("swarmReadOnly", true)?,
            parallelism.clamp(1, MAX_SWARM_PARALLELISM as i32) as u8,
        )
    } else {
        (true, 1)
    };
    Ok(Action::Ai(ImportedAiAction {
        kind,
        goal,
        profile_id,
        profile_source: None,
        working_directory: None,
        parallelism,
        swarm_read_only,
    }))
}

fn parse_time(value: &str) -> ParseResult<Time> {
    value
        .parse::<Time>()
        .map_err(|_| Omission::InvalidDefinition)
}

fn parse_date(value: &str) -> ParseResult<Date> {
    value
        .parse::<Date>()
        .map_err(|_| Omission::InvalidDefinition)
}

fn job_id(source: &str) -> ParseResult<JobId> {
    let id = compact_uuid(source).ok_or(Omission::InvalidDefinition)?;
    JobId::new(format!("java-{id}")).map_err(|_| Omission::InvalidDefinition)
}

fn compact_uuid(source: &str) -> Option<String> {
    (source.len() == 36
        && source.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        }))
    .then(|| source.replace('-', "").to_ascii_lowercase())
}

fn source_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SOURCE_ID_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

fn bounded(value: &str, limit: usize, multiline: bool) -> ParseResult<&str> {
    if value.trim().is_empty()
        || value.len() > limit
        || value.chars().any(|character| {
            !crate::session_journal::is_display_character(character)
                || (!multiline && matches!(character, '\n' | '\t'))
        })
    {
        return Err(Omission::InvalidDefinition);
    }
    Ok(value)
}

struct Fields<'a, 'input>(BTreeMap<&'input str, Node<'a, 'input>>);

impl<'a, 'input> Fields<'a, 'input> {
    fn new(node: Node<'a, 'input>) -> ParseResult<Self> {
        if node.attributes().len() != 0 {
            return Err(Omission::InvalidDefinition);
        }
        let mut fields = BTreeMap::new();
        for child in node.children().filter(|child| !ignorable(*child)) {
            let name = child.tag_name();
            if !child.is_element()
                || name.namespace().is_some()
                || fields.insert(name.name(), child).is_some()
            {
                return Err(Omission::InvalidDefinition);
            }
        }
        Ok(Self(fields))
    }

    fn get(&self, name: &str) -> Option<Node<'a, 'input>> {
        self.0.get(name).copied()
    }

    fn text(&self, name: &str) -> ParseResult<&'a str> {
        self.optional_text(name)?.ok_or(Omission::InvalidDefinition)
    }

    fn optional_text(&self, name: &str) -> ParseResult<Option<&'a str>> {
        self.get(name)
            .map(element_text)
            .transpose()
            .map(|value| value.map(str::trim).filter(|value| !value.is_empty()))
    }

    fn java_text(&self, name: &str) -> ParseResult<Option<&'a str>> {
        self.get(name).map(element_text).transpose().map(|value| {
            value
                .map(trim_java_list_item)
                .filter(|value| !value.is_empty())
        })
    }

    fn list(&self, name: &str, item: &str, limit: usize) -> ParseResult<Vec<Node<'a, 'input>>> {
        let Some(wrapper) = self.get(name) else {
            return Ok(Vec::new());
        };
        if wrapper.attributes().len() != 0 {
            return Err(Omission::InvalidDefinition);
        }
        let mut items = Vec::new();
        for child in wrapper.children().filter(|child| !ignorable(*child)) {
            if items.len() == limit
                || !child.is_element()
                || child.tag_name().namespace().is_some()
                || !child.has_tag_name(item)
            {
                return Err(Omission::InvalidDefinition);
            }
            items.push(child);
        }
        Ok(items)
    }

    fn text_list(&self, name: &str, item: &str, limit: usize) -> ParseResult<Vec<String>> {
        let mut values = Vec::new();
        for node in self.list(name, item, limit)? {
            let value = element_text(node)?.trim();
            if !value.is_empty() {
                values.push(value.to_owned());
            }
        }
        Ok(values)
    }

    fn boolean(&self, name: &str, fallback: bool) -> ParseResult<bool> {
        match self.optional_text(name)? {
            None => Ok(fallback),
            Some("true" | "1") => Ok(true),
            Some("false" | "0") => Ok(false),
            _ => Err(Omission::InvalidDefinition),
        }
    }

    fn integer(&self, name: &str, fallback: i32) -> ParseResult<i32> {
        self.optional_text(name)?.map_or(Ok(fallback), |value| {
            value.parse().map_err(|_| Omission::InvalidDefinition)
        })
    }

    fn positive_minutes(&self, name: &str) -> ParseResult<Option<NonZeroU32>> {
        self.optional_text(name)?
            .map(|value| {
                value
                    .parse::<i32>()
                    .map_err(|_| Omission::InvalidDefinition)
                    .map(|value| u32::try_from(value).ok().and_then(NonZeroU32::new))
            })
            .transpose()
            .map(Option::flatten)
    }
}

fn ignorable(node: Node<'_, '_>) -> bool {
    node.is_comment() || node.is_text() && node.text().is_none_or(|text| text.trim().is_empty())
}

fn element_text<'a>(node: Node<'a, '_>) -> ParseResult<&'a str> {
    if node.attributes().len() != 0 {
        return Err(Omission::InvalidDefinition);
    }
    let mut content = None;
    for child in node.children().filter(|child| !child.is_comment()) {
        if !child.is_text() || content.replace(child.text().unwrap_or_default()).is_some() {
            return Err(Omission::InvalidDefinition);
        }
    }
    Ok(content.unwrap_or_default())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ssh_dialog::LoginTarget;
    use roxmltree::Document;

    const XML: &str = include_str!("java_scheduler/fixtures/java-2.26.0.xml");
    const PRIMARY: &str = "b04d413f-9ee5-4b56-bc12-c72da52d4b3e";
    const SECONDARY: &str = "9c0719d8-dfa7-4468-a1d9-4149cb8fa01e";

    fn connection(id: u64, host: &str) -> ConnectionRef {
        ConnectionRef {
            teamwork: None,
            temporary_keys: None,
            id,
            target: LoginTarget {
                host: host.into(),
                port: 22,
                username: "fixture".into(),
            },
            jump: None,
        }
    }

    #[test]
    fn java_scheduler_backup_reads_pinned_jaxb_jobs_and_binds_reviewed_targets() -> io::Result<()> {
        let review = inspect(XML.as_bytes())?;
        assert_eq!(review.unsupported_sections, 1);
        assert!(review.omitted.is_empty());
        assert_eq!(review.privileged_drafts(), 1);
        let [command, snippet, privileged, dependent, grouped, agent] = review
            .jobs
            .try_into()
            .unwrap_or_else(|_| panic!("expected all six reference jobs"));
        assert_eq!(
            command.definition.id.as_str(),
            "java-3f9bab16184b4e13bf71e37331b8843a"
        );
        assert_eq!(command.definition.name, "Inspect café & 東京");
        assert!(
            !command.definition.enabled && !snippet.definition.enabled && !agent.definition.enabled
        );
        assert_eq!(command.source_targets, [PRIMARY]);
        assert_eq!(snippet.source_targets, [PRIMARY, SECONDARY]);
        assert_eq!(
            snippet.definition.dependencies.as_slice(),
            std::slice::from_ref(&command.definition.id)
        );
        assert_eq!(snippet.definition.mode, DependencyMode::Chain);
        assert_eq!(
            snippet.definition.max_age_minutes.map(NonZeroU32::get),
            Some(35)
        );
        let schedule = command.definition.schedule.as_ref().unwrap();
        assert!(schedule.enabled());
        assert_eq!(
            schedule.weekdays().collect::<Vec<_>>(),
            [Weekday::Monday, Weekday::Friday]
        );
        assert_eq!(
            schedule.fixed_times().collect::<Vec<_>>(),
            [
                Time::new(0, 15, 0, 0).unwrap(),
                Time::new(22, 15, 0, 0).unwrap()
            ]
        );
        assert_eq!(
            schedule.window(),
            (
                Time::new(22, 0, 0, 0).unwrap(),
                Time::new(2, 0, 0, 0).unwrap()
            )
        );
        assert_eq!(
            schedule.active_dates(),
            (
                Some(Date::new(2026, 11, 1).unwrap()),
                Some(Date::new(2027, 2, 1).unwrap())
            )
        );
        assert_eq!(schedule.interval_minutes().map(NonZeroU32::get), Some(30));

        let command = command
            .bind(
                |id| (id == PRIMARY).then(|| connection(17, "first.example")),
                |_| None,
                |_| None,
                |_| false,
            )
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(command.action().command, "printf '%s\\n' 'fixture & data'");
        assert_eq!(
            command.action().working_directory.as_deref(),
            Some("/srv/project files")
        );
        assert_eq!(command.action().connection.as_ref().unwrap().id, 17);
        assert_eq!(command.action().sudo, SudoMode::Disabled);
        assert_eq!(
            command.action().journal_detail,
            JournalDetail::LimitedRedacted
        );
        assert!(command.action().webhook_targets.is_empty());
        let snippet = snippet
            .bind(
                |id| match id {
                    PRIMARY => Some(connection(17, "first.example")),
                    SECONDARY => Some(connection(33, "second.example")),
                    _ => None,
                },
                |_| None,
                |_| None,
                |id| String::from(id) == "3ee665452eaf4ac7b352d9a1a51d75ed",
            )
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(snippet.action().additional_connections[0].id, 33);
        assert_eq!(
            snippet.action().snippet.as_ref().unwrap().arguments,
            ["literal space", "a'b"]
        );
        let primary = || connection(17, "first.example");
        let bind_primary = |job: ImportedJob| {
            job.bind(
                |id| (id == PRIMARY).then(primary),
                |_| None,
                |_| None,
                |_| false,
            )
            .unwrap()
            .pop()
            .unwrap()
        };
        let privileged = bind_primary(privileged);
        assert!(!privileged.enabled());
        assert_eq!(privileged.action().sudo, SudoMode::Disabled);
        assert_eq!(
            privileged.action().require_executable(),
            Err(imported_privileged::REVIEW_REQUIRED)
        );
        let draft = privileged.action().imported_privileged.as_ref().unwrap();
        assert_eq!(draft.routes, [primary()]);
        assert!(matches!(
            &draft.settings,
            imported_privileged::Settings::Command { text, options }
                if text == "whoami" && options == &Default::default()
        ));
        let dependent = bind_primary(dependent);
        assert!(!dependent.enabled());
        assert_eq!(
            dependent.dependencies(),
            std::slice::from_ref(privileged.id())
        );
        assert_eq!(dependent.action().command, "true");
        assert!(grouped.source_targets.is_empty());
        assert_eq!(grouped.source_groups, ["Ops/Hosts"]);
        let grouped = grouped
            .bind(
                |_| None,
                |group| {
                    (group == "Ops/Hosts")
                        .then(|| vec![(SECONDARY.into(), connection(33, "second.example"))])
                },
                |_| None,
                |_| false,
            )
            .unwrap()
            .pop()
            .unwrap();
        let selection = grouped.action().target_groups.as_ref().unwrap();
        assert_eq!(
            selection.groups,
            [crate::scheduler_window::ConnectionGroup::from("Ops/Hosts")]
        );
        assert!(selection.explicit.is_empty());
        assert_eq!(grouped.action().connection.as_ref().unwrap().id, 33);
        let agent = agent
            .bind(
                |id| (id == PRIMARY).then(|| connection(17, "first.example")),
                |_| None,
                |_| None,
                |_| false,
            )
            .unwrap()
            .pop()
            .unwrap();
        let draft = agent.action().imported_ai.as_ref().unwrap();
        assert_eq!(draft.kind, Kind::Agent);
        assert_eq!(draft.goal, "Inspect the fixture");
        assert!(draft.profile_id.is_none());
        assert!(agent.action().agent.is_none());
        assert!(agent.action().command.is_empty());
        assert!(
            JobGraph::new(vec![
                command, snippet, privileged, dependent, grouped, agent
            ])
            .is_ok()
        );
        Ok(())
    }

    #[test]
    fn java_scheduler_groups_keep_order_and_legacy_explicit_selection() -> io::Result<()> {
        let xml = |targets: &str, groups: &str, pty: bool| {
            format!(
                r#"<jobScheduler><jobs><job>
                    <id>7ec12ac5-5d6e-496b-947c-deb608c9f11d</id><name>Grouped job</name>
                    {targets}<targetGroupNames>{groups}</targetGroupNames>
                    <action><type>COMMAND</type><command>uptime</command>
                    <ptyEnabled>{pty}</ptyEnabled></action>
                </job></jobs></jobScheduler>"#
            )
        };
        let targets = format!(
            "<connectionId>{PRIMARY}</connectionId>
             <targetConnectionIds><connectionId> &#9; </connectionId></targetConnectionIds>"
        );
        let groups = "<groupName> / Ops // Hosts / </groupName><groupName>Ops/Hosts</groupName>
                      <groupName> </groupName><groupName>Ops/Other</groupName>";
        let review = inspect(xml(&targets, groups, false).as_bytes())?;
        assert_eq!(review.jobs[0].source_targets, [PRIMARY]);
        assert_eq!(review.jobs[0].source_groups, ["Ops/Hosts", "Ops/Other"]);
        let explicit = connection(17, "first.example");
        let second = connection(33, "second.example");
        let third = connection(51, "third.example");
        let bound = review.bind_all(
            |id| (id == PRIMARY).then(|| explicit.clone()),
            |group| match group {
                "Ops/Hosts" => Some(vec![
                    (SECONDARY.into(), second.clone()),
                    (PRIMARY.into(), explicit.clone()),
                ]),
                "Ops/Other" => Some(vec![
                    (SECONDARY.into(), second.clone()),
                    ("third".into(), third.clone()),
                ]),
                _ => None,
            },
            |_| None,
            |_| false,
        )?;
        let stored = decode(&encode(&bound.jobs)?)?;
        let action = stored.jobs()[0].action();
        assert!(bound.omitted.is_empty());
        assert!(!stored.jobs()[0].enabled());
        assert_eq!(
            action.targets().collect::<Vec<_>>(),
            [&explicit, &second, &third]
        );
        let groups = action.target_groups.as_ref().unwrap();
        assert_eq!(groups.explicit.as_slice(), std::slice::from_ref(&explicit));
        assert_eq!(
            groups.groups,
            ["Ops/Hosts", "Ops/Other"].map(crate::scheduler_window::ConnectionGroup::from)
        );

        let root = inspect(xml(&targets, "<groupName> / / </groupName>", false).as_bytes())?;
        assert_eq!(root.omitted, BTreeMap::from([(Omission::GroupTargets, 1)]));
        let singular = format!("<connectionId> {PRIMARY} </connectionId>");
        let invalid =
            inspect(xml(&singular, "<groupName>Ops/Hosts</groupName>", false).as_bytes())?;
        assert_eq!(
            invalid.omitted,
            BTreeMap::from([(Omission::InvalidTargets, 1)])
        );

        let group = "<groupName>Ops/Hosts</groupName>";
        let pty = xml("", group, true);
        let one = inspect(pty.as_bytes())?.bind_all(
            |_| None,
            |_| Some(vec![(PRIMARY.into(), explicit.clone())]),
            |_| None,
            |_| false,
        )?;
        assert!(one.jobs.jobs()[0].action().virtual_terminal.is_some());
        let two = inspect(pty.as_bytes())?.bind_all(
            |_| None,
            |_| {
                Some(vec![
                    (PRIMARY.into(), explicit.clone()),
                    (SECONDARY.into(), second.clone()),
                ])
            },
            |_| None,
            |_| false,
        )?;
        assert!(two.jobs.jobs().is_empty());
        assert_eq!(
            two.omitted,
            BTreeMap::from([(Omission::UnsupportedExecutionOptions, 1)])
        );
        Ok(())
    }

    #[test]
    fn java_scheduler_imports_full_redacted_output_for_commands() -> io::Result<()> {
        let xml = XML.replacen("LIMITED_REDACTED", "FULL", 1);
        let review = inspect(xml.as_bytes())?;
        assert_eq!(review.jobs[0].journal_detail, JournalDetail::Full);
        assert_eq!(
            review.jobs[1].journal_detail,
            JournalDetail::LimitedRedacted
        );
        assert_eq!(
            review.jobs.last().unwrap().journal_detail,
            JournalDetail::Off
        );
        Ok(())
    }

    #[test]
    fn java_scheduler_virtual_terminal_maps_defaults_and_java_clamped_values() -> io::Result<()> {
        let import = |controls| {
            let xml = XML.replacen("<ptyEnabled>false</ptyEnabled>", controls, 1);
            let reviewed = inspect(xml.as_bytes())?;
            reviewed
                .jobs
                .into_iter()
                .next()
                .unwrap()
                .bind(
                    |_| Some(connection(17, "first.example")),
                    |_| None,
                    |_| None,
                    |_| false,
                )
                .map(|mut jobs| jobs.pop().unwrap())
                .map_err(|_| invalid("Could not bind the fixture job."))
        };
        let defaults = import("<ptyEnabled>1</ptyEnabled>")?;
        assert!(!defaults.enabled());
        assert_eq!(
            defaults.action().virtual_terminal,
            Some(TerminalOptions::default())
        );
        let clamped = import(
            "<ptyEnabled>true</ptyEnabled>
             <ptyColumns>-2147483648</ptyColumns><ptyRows>2147483647</ptyRows>
             <screenshotIntervalSeconds>3601</screenshotIntervalSeconds>
             <screenshotOnChange>0</screenshotOnChange><maxScreenshots>-1</maxScreenshots>
             <runtimeLimitSeconds>100000</runtimeLimitSeconds>",
        )?;
        assert_eq!(
            clamped.action().virtual_terminal,
            Some(TerminalOptions {
                columns: 40,
                rows: 200,
                interval_seconds: 3600,
                capture_changes: false,
                max_frames: 1,
                limit_seconds: 86_400,
                limit_is_success: false,
            }),
        );
        let inactive = import(
            "<ptyEnabled>false</ptyEnabled><runtimeLimitSeconds>0</runtimeLimitSeconds>
             <ptyColumns>unused</ptyColumns>",
        )?;
        assert!(inactive.action().virtual_terminal.is_none());
        Ok(())
    }

    #[test]
    fn java_scheduler_virtual_terminal_omits_unsupported_and_invalid_jobs() -> io::Result<()> {
        let command = "<command>pwd</command>";
        let single = format!("<connectionId>{PRIMARY}</connectionId>");
        let multiple = format!(
            "<targetConnectionIds><connectionId>{PRIMARY}</connectionId>\
             <connectionId>{SECONDARY}</connectionId></targetConnectionIds>"
        );
        for (kind, body, routes, options, reason) in [
            (
                "COMMAND",
                command,
                multiple.as_str(),
                "",
                Omission::UnsupportedExecutionOptions,
            ),
            (
                "AI_AGENT",
                "<aiPrompt>Inspect the fixture</aiPrompt>",
                single.as_str(),
                "",
                Omission::UnsupportedExecutionOptions,
            ),
            (
                "COMMAND",
                command,
                single.as_str(),
                "<useSudo>true</useSudo><sudoStagingEnabled>true</sudoStagingEnabled>",
                Omission::UnsupportedExecutionOptions,
            ),
            (
                "COMMAND",
                command,
                single.as_str(),
                "<ptyRows>2147483648</ptyRows>",
                Omission::InvalidDefinition,
            ),
            (
                "COMMAND",
                command,
                single.as_str(),
                "<screenshotOnChange>yes</screenshotOnChange>",
                Omission::InvalidDefinition,
            ),
        ] {
            let source = format!(
                "<jobScheduler><jobs><job><id>3f9bab16-184b-4e13-bf71-e37331b8843a</id>\
                 <name>Virtual terminal fixture</name>{routes}<action><type>{kind}</type>\
                 {body}<ptyEnabled>true</ptyEnabled>{options}</action></job></jobs></jobScheduler>"
            );
            let reviewed = inspect(source.as_bytes())?;
            assert_eq!(reviewed.omitted, BTreeMap::from([(reason, 1)]));
            assert!(reviewed.jobs.is_empty());
        }
        Ok(())
    }

    #[test]
    fn java_scheduler_backup_preserves_reviewable_swarm_modes_and_target_capacity() -> io::Result<()>
    {
        let targets = (0..MAX_SWARM_TARGETS)
            .map(|index| format!("<connectionId>target-{index}</connectionId>"))
            .collect::<String>();
        let xml = |mode: &str, extra: &str, targets: &str| {
            format!(
                r#"<jobScheduler><jobs><job>
                <id>2bd70fbe-a5e4-4b29-b621-370fcce50b96</id><name>Swarm fixture</name>
                <enabled>true</enabled>
                <targetConnectionIds>{targets}</targetConnectionIds>
                <workingDirectory>relative workspace</workingDirectory>
                <action><type>{mode}</type>
                    <aiPrompt>Inspect café
Keep it read-only</aiPrompt>
                    <aiProfileId> fixture-profile </aiProfileId>
                    <aiAutoApproveCommands>true</aiAutoApproveCommands>
                    {extra}
                </action>
            </job></jobs></jobScheduler>"#
            )
        };
        let resolve = |id: &str| {
            let index = id.strip_prefix("target-")?.parse::<u64>().ok()?;
            Some(connection(index + 1, "fixture.example"))
        };
        for (text, expected_mode, expected_parallelism) in [
            ("", true, 4),
            (
                "<swarmReadOnly>false</swarmReadOnly><swarmMaxParallelism>42</swarmMaxParallelism>",
                false,
                16,
            ),
            ("<swarmMaxParallelism>-1</swarmMaxParallelism>", true, 1),
        ] {
            let review = inspect(xml("AI_SWARM", text, &targets).as_bytes())?;
            let bound = review.bind_all(resolve, |_| None, |_| None, |_| false)?;
            let bytes = encode(&bound.jobs)?;
            let stored = decode(&bytes)?;
            let job = &stored.jobs()[0];
            assert!(!job.enabled());
            assert_eq!(job.action().targets().count(), MAX_SWARM_TARGETS);
            let draft = job.action().imported_ai.as_ref().unwrap();
            assert_eq!(draft.kind, Kind::Swarm);
            assert_eq!(draft.goal, "Inspect café\nKeep it read-only");
            assert_eq!(draft.profile_id.as_deref(), Some("fixture-profile"));
            assert_eq!(
                draft.working_directory.as_deref(),
                Some("relative workspace")
            );
            assert_eq!(draft.swarm_read_only, expected_mode);
            assert_eq!(draft.parallelism, expected_parallelism);
            assert!(job.action().agent.is_none());
            assert!(job.action().working_directory.is_none());

            let mut forged: serde_json::Value = serde_json::from_slice(&bytes)?;
            let action = &mut forged["actions"][job.id().as_str()];
            action["command"] = "whoami".into();
            assert!(decode(&serde_json::to_vec(&forged)?).is_err());
        }

        let too_many = format!("{targets}<connectionId>extra</connectionId>");
        assert!(
            inspect(xml("AI_SWARM", "", &too_many).as_bytes())?
                .jobs
                .is_empty()
        );
        assert!(
            inspect(xml("AI_AGENT", "", &targets).as_bytes())?
                .jobs
                .is_empty()
        );
        let grouped = xml("AI_SWARM", "", "").replace(
            "</targetConnectionIds>",
            "</targetConnectionIds><targetGroupNames><groupName>Ops/Hosts</groupName></targetGroupNames>",
        );
        let members: Vec<_> = (1..=MAX_SWARM_TARGETS as u64)
            .map(|id| (format!("member-{id}"), connection(id, "fixture.example")))
            .collect();
        let grouped_bound = inspect(grouped.as_bytes())?.bind_all(
            |_| None,
            |_| Some(members.clone()),
            |_| None,
            |_| false,
        )?;
        let stored = decode(&encode(&grouped_bound.jobs)?)?;
        let job = &stored.jobs()[0];
        assert!(!job.enabled());
        assert_eq!(job.action().targets().count(), MAX_SWARM_TARGETS);
        assert!(job.action().require_executable().is_err());
        let overflow = inspect(grouped.as_bytes())?.bind_all(
            |_| None,
            |_| {
                let mut members = members.clone();
                members.push((
                    "extra".into(),
                    connection(MAX_SWARM_TARGETS as u64 + 1, "fixture.example"),
                ));
                Some(members)
            },
            |_| None,
            |_| false,
        )?;
        assert!(overflow.jobs.jobs().is_empty());
        assert_eq!(
            overflow.omitted,
            BTreeMap::from([(Omission::InvalidTargets, 1)])
        );
        let missing_prompt =
            xml("AI_SWARM", "", &targets).replace("Inspect café\nKeep it read-only", " ");
        assert!(inspect(missing_prompt.as_bytes())?.jobs.is_empty());
        Ok(())
    }

    #[test]
    fn java_scheduler_backup_preserves_journal_options_and_defers_profile_binding() -> io::Result<()>
    {
        let xml = XML.replacen(
            "<schedule>",
            r#"<sessionJournal>
                <enabled>true</enabled>
                <captureInput>false</captureInput>
                <aiMode>ON_FAILURE</aiMode>
                <aiProfileId>fixture-java-profile</aiProfileId>
                <keepMode>ONLY_ON_FAILURE</keepMode>
                <retentionMode>FIXED_DATE</retentionMode>
                <retentionDays>37</retentionDays>
                <expiryDate>2027-01-31</expiryDate>
                <maxJournals>17</maxJournals>
                <maxStorageMb>128</maxStorageMb>
                <dedupEnabled>false</dedupEnabled>
            </sessionJournal>
            <schedule>"#,
            1,
        );
        let review = inspect(xml.as_bytes())?;
        assert_eq!(review.unresolved_journal_profiles(), 1);
        let reviewed = review.bind_all(
            |id| match id {
                PRIMARY => Some(connection(17, "first.example")),
                SECONDARY => Some(connection(33, "second.example")),
                _ => None,
            },
            |_| None,
            |_| None,
            |_| true,
        )?;
        let bytes = encode(&reviewed.jobs)?;
        let restored = decode(&bytes)?;
        let job = &restored.jobs()[0];
        let options = Options {
            enabled: true,
            capture_input: false,
            keep: Keep::Failures,
            closing: Closing::Failures,
            summary_profile: None,
            unresolved_summary_profile: Some("fixture-java-profile".into()),
            java_profile_source: None,
            retention: Retention::Date {
                date: "2027-01-31".into(),
            },
            max_runs: 17,
            max_storage_mib: 128,
            deduplicate: false,
        };
        assert!(!job.enabled());
        assert_eq!(job.action().automation_journal.as_ref(), Some(&options));
        assert!(restored.jobs()[1].action().automation_journal.is_none());

        let mut forged: serde_json::Value = serde_json::from_slice(&bytes)?;
        let journal = &mut forged["actions"][job.id().as_str()]["automation_journal"];
        journal
            .as_object_mut()
            .unwrap()
            .remove("unresolved_summary_profile");
        journal["summary_profile"] = serde_json::json!({
            "id": 8, "model": "fixture", "destination": "127.0.0.1:11434"
        });
        assert!(decode(&serde_json::to_vec(&forged)?).is_err());
        Ok(())
    }

    #[test]
    fn java_scheduler_journal_defaults_and_limits_match_the_supported_shape() {
        let parse = |xml| {
            let doc = Document::parse(xml).unwrap();
            parse_session_journal(doc.root_element())
        };
        let defaults = parse("<sessionJournal/>").unwrap();
        assert!(!defaults.enabled);
        assert!(defaults.capture_input);
        assert_eq!(defaults.closing, Closing::Always);
        assert_eq!(defaults.retention, Retention::Days { days: 14 });
        assert!(defaults.deduplicate);
        let fixed = parse(
            "<sessionJournal><retentionMode>FIXED_DATE</retentionMode><expiryDate>bad</expiryDate><retentionDays>0</retentionDays></sessionJournal>",
        )
        .unwrap();
        assert_eq!(fixed.retention, Retention::Days { days: 1 });
        let forever = parse(
            "<sessionJournal><retentionMode>NONE</retentionMode><aiMode>OFF</aiMode><maxJournals>-1</maxJournals></sessionJournal>",
        )
        .unwrap();
        assert_eq!(forever.closing, Closing::Off);
        assert_eq!(forever.retention, Retention::Forever);
        assert_eq!(forever.max_runs, 0);
        assert!(parse("<sessionJournal><maxJournals>1001</maxJournals></sessionJournal>").is_err());
        assert!(
            parse(
                "<sessionJournal><enabled>true</enabled><enabled>false</enabled></sessionJournal>"
            )
            .is_err()
        );
    }

    #[test]
    fn java_scheduler_backup_rejects_ambiguous_ids_unsafe_xml_and_missing_targets() -> io::Result<()>
    {
        let root = XML.split_once("<jobs>").unwrap().1;
        let first = root.split_once("</job>").unwrap().0;
        let duplicated =
            format!("<jobScheduler><jobs>{first}</job>{first}</job></jobs></jobScheduler>");
        assert!(inspect(duplicated.as_bytes()).is_err());
        let external =
            br#"<!DOCTYPE jobScheduler [<!ENTITY f SYSTEM "file:///secret">]><jobScheduler/>"#;
        assert!(inspect(external).is_err());
        let altered = XML.replace(
            "<hostKeyVerificationDisabled>false</hostKeyVerificationDisabled>",
            "<hostKeyVerificationDisabled>true</hostKeyVerificationDisabled>",
        );
        assert!(inspect(altered.as_bytes())?.jobs.is_empty());
        let mut jobs = inspect(XML.as_bytes())?.jobs.into_iter();
        assert!(
            jobs.next()
                .unwrap()
                .bind(|_| None, |_| None, |_| None, |_| false)
                .is_err()
        );
        assert!(
            jobs.next()
                .unwrap()
                .bind(
                    |id| Some(connection(
                        if id == PRIMARY { 17 } else { 33 },
                        "fixture.example"
                    )),
                    |_| None,
                    |_| None,
                    |_| false
                )
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn java_scheduler_backup_serializes_only_closed_disabled_jobs_without_authority()
    -> io::Result<()> {
        let lookup = |id: &str| match id {
            PRIMARY => Some(connection(17, "first.example")),
            SECONDARY => Some(connection(33, "second.example")),
            _ => None,
        };
        let reviewed = inspect(XML.as_bytes())?.bind_all(lookup, |_| None, |_| None, |_| true)?;
        assert_eq!(reviewed.jobs.jobs().len(), 5);
        assert_eq!(reviewed.omitted[&Omission::GroupTargets], 1);
        let bytes = encode(&reviewed.jobs)?;
        let restored = decode(&bytes)?;
        for (expected, actual) in reviewed.jobs.jobs().iter().zip(restored.jobs()) {
            assert_eq!(expected.definition(), actual.definition());
            assert_eq!(expected.action(), actual.action());
        }

        let mut forged: serde_json::Value = serde_json::from_slice(&bytes)?;
        forged["definitions"][0]["enabled"] = serde_json::json!(true);
        assert!(decode(&serde_json::to_vec(&forged)?).is_err());
        forged["definitions"][0]["enabled"] = serde_json::json!(false);
        let command_id = reviewed.jobs.jobs()[0].id().as_str();
        forged["actions"][command_id]["sudo"] = serde_json::json!("passwordless");
        assert!(decode(&serde_json::to_vec(&forged)?).is_err());

        let missing_target = inspect(XML.as_bytes())?.bind_all(
            |id| (id == SECONDARY).then(|| connection(33, "second.example")),
            |_| None,
            |_| None,
            |_| true,
        )?;
        assert!(missing_target.jobs.jobs().is_empty());
        assert_eq!(missing_target.omitted[&Omission::InvalidTargets], 5);
        let missing_snippet =
            inspect(XML.as_bytes())?.bind_all(lookup, |_| None, |_| None, |_| false)?;
        assert_eq!(missing_snippet.jobs.jobs().len(), 4);
        assert_eq!(missing_snippet.omitted[&Omission::MissingSnippet], 1);

        let parent_unavailable = XML.replacen(
            "<connectionId>b04d413f-9ee5-4b56-bc12-c72da52d4b3e</connectionId>",
            "<connectionId>missing-parent-target</connectionId>",
            1,
        );
        let omitted = inspect(parent_unavailable.as_bytes())?.bind_all(
            lookup,
            |_| None,
            |_| None,
            |_| true,
        )?;
        assert_eq!(omitted.jobs.jobs().len(), 3);
        assert!(
            omitted
                .jobs
                .jobs()
                .iter()
                .any(|job| job.action().imported_ai.is_some())
        );
        assert!(
            omitted
                .jobs
                .jobs()
                .iter()
                .any(|job| job.action().imported_privileged.is_some())
        );
        assert_eq!(omitted.omitted[&Omission::InvalidTargets], 1);
        assert_eq!(omitted.omitted[&Omission::InvalidDependency], 1);
        Ok(())
    }

    #[test]
    fn java_scheduler_archive_fanout_limit_preserves_unrelated_jobs() -> io::Result<()> {
        use std::fmt::Write as _;

        let mut xml = String::from("<jobScheduler><jobs>");
        let source_id = |number: usize| format!("7ec12ac5-5d6e-496b-947c-{number:012x}");
        write!(
            xml,
            "<job><id>{}</id><name>Archive</name><targetConnectionIds>
                <connectionId>{PRIMARY}</connectionId><connectionId>{SECONDARY}</connectionId>
                </targetConnectionIds><action><type>SFTP_ARCHIVE</type>
                <archiveSources><path>/srv/files</path></archiveSources>
                <archivePath>/backups/copy.zip</archivePath></action></job>",
            source_id(0)
        )
        .unwrap();
        for number in 1..MAX_JOBS {
            let depends = if number == 1 {
                format!("<dependsOn><jobId>{}</jobId></dependsOn>", source_id(0))
            } else {
                String::new()
            };
            write!(
                xml,
                "<job><id>{}</id><name>Run {number}</name><connectionId>{PRIMARY}</connectionId>
                    {depends}<action><type>COMMAND</type><command>printf ready</command></action></job>",
                source_id(number)
            )
            .unwrap();
        }
        xml.push_str("</jobs></jobScheduler>");
        let review = inspect(xml.as_bytes())?;
        assert_eq!(review.jobs.len(), MAX_JOBS);
        assert!(review.omitted.is_empty());
        let bound = review.bind_all(
            |id| match id {
                PRIMARY => Some(connection(17, "first.example")),
                SECONDARY => Some(connection(33, "second.example")),
                _ => None,
            },
            |_| None,
            |_| None,
            |_| false,
        )?;
        assert_eq!(bound.jobs.jobs().len(), MAX_JOBS - 2);
        assert_eq!(bound.omitted[&Omission::TargetFanoutLimit], 1);
        assert_eq!(bound.omitted[&Omission::InvalidDependency], 1);
        assert!(
            bound
                .jobs
                .jobs()
                .iter()
                .all(|job| job.dependencies().is_empty() && !job.enabled())
        );
        Ok(())
    }
}
