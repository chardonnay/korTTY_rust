use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::OnceLock;

use iced::widget::{
    button, checkbox, column, pick_list, row, scrollable, text, text_editor, text_input,
};
use iced::{Element, Length};
use kortty_jobs::{
    Date, DependencyMode, Job, JobDefinition, JobGraph, JobId, JobSchedule, Time, Weekday,
};

use crate::ai_manager::ProfileRef;
use crate::connection_store::Store as ConnectionStore;
use crate::connection_window::CredentialChoice;
use crate::job_executor::virtual_terminal::Options as TerminalOptions;
use crate::sftp_window::privileged_file::ProtectedOperation;
use crate::snippets::SnippetId;

use super::privileged::{
    PrivilegedArchive, PrivilegedDirection, PrivilegedFileAction, PrivilegedTransfer,
};
use super::remote::{
    Archive, ArchiveFormat, MAX_ARCHIVE_EXCLUDE_TEXT, MAX_ARCHIVE_SOURCE_TEXT, RemoteAction,
    RemotePath, mode_from_text, owner_id_from_text,
};
use super::script::{
    Language, MAX_ARGUMENT_TEXT_BYTES, MAX_COMMAND_BYTES, Script, SnippetLink, arguments_from_lines,
};
use super::sync::standing::{Limits, MAX_BYTES, Settings};
use super::working_directory::parent_path;
use super::{
    ConnectionChoice, ConnectionRef, DirectoryField, Message, SnippetChoice, SshCommand, SudoMode,
    TargetGroups,
};

const DAYS: [(Weekday, &str); 7] = [
    (Weekday::Monday, "Mon"),
    (Weekday::Tuesday, "Tue"),
    (Weekday::Wednesday, "Wed"),
    (Weekday::Thursday, "Thu"),
    (Weekday::Friday, "Fri"),
    (Weekday::Saturday, "Sat"),
    (Weekday::Sunday, "Sun"),
];
const MAX_FIELD_BYTES: usize = 4096;
const MIB: f64 = 1_048_576.0;
const SWARM_PARALLELISM_CHOICES: [u8; kortty_jobs::MAX_SWARM_PARALLELISM] =
    [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];

#[derive(Clone, Copy, Debug)]
pub enum Field {
    Name,
    Host,
    Port,
    Username,
    KeyFile,
    WorkingDirectory,
    RemotePath,
    RemoteDestination,
    RemoteMode,
    RemoteUid,
    RemoteGid,
    ArchiveLevel,
    ArchiveDownload,
    RemoteDeleteConfirmation,
    LocalFile,
    SyncStandingDays,
    SyncStandingFirings,
    SyncStandingActions,
    SyncStandingMib,
    RsyncProgram,
    AgentGoal,
    SwarmParallelism,
    From,
    Until,
    WindowStart,
    WindowEnd,
    Interval,
    Age,
    TerminalColumns,
    TerminalRows,
    TerminalInterval,
    TerminalMaxFrames,
    TerminalLimit,
}

pub(super) struct ActionPermissions {
    pub(super) browse: bool,
    pub(super) privileged: bool,
    pub(super) automation_journal: bool,
}

pub(super) struct Draft {
    pub id: Option<JobId>,
    name: String,
    host: String,
    port: String,
    username: String,
    key_file: String,
    connection: Option<ConnectionRef>,
    additional_connections: Vec<ConnectionRef>,
    target_groups: Option<TargetGroups>,
    group_targets_unresolved: bool,
    working_directory: String,
    command: text_editor::Content,
    pub source: Source,
    pub script_language: Language,
    pub snippet_id: Option<SnippetId>,
    script_arguments: text_editor::Content,
    archive_sources: text_editor::Content,
    archive_excludes: text_editor::Content,
    sync_excludes: text_editor::Content,
    sync_approval: Option<String>,
    sync_replace_changed: bool,
    sync_standing: bool,
    sync_standing_days: String,
    sync_standing_firings: String,
    sync_standing_actions: String,
    sync_standing_mib: String,
    rsync_program: String,
    rsync_approval: Option<super::rsync::Approval>,
    agent_goal: String,
    agent_profile: Option<ProfileRef>,
    pub(super) agent_probe: super::agent::Probe,
    swarm_parallelism: String,
    agent_approval: Option<Box<super::agent::Approval>>,
    pub(super) archive_format: ArchiveFormat,
    pub(super) archive_server_only: bool,
    pub(super) archive_password: Option<String>,
    archive_level: String,
    archive_download: String,
    remote_path: String,
    remote_destination: String,
    remote_mode: String,
    remote_uid: String,
    remote_gid: String,
    remote_delete_confirmation: String,
    recursive_delete: bool,
    privileged_copy_tree: bool,
    local_file: String,
    privileged_approval: Option<String>,
    pub enabled: bool,
    pub schedule_enabled: bool,
    pub virtual_terminal: TerminalDraft,
    pub(super) automation_journal: super::automation_journal::Draft,
    pub(super) desktop_notices: super::notifications::Settings,
    pub(super) webhook_targets: Vec<super::webhooks::Target>,
    pub(super) sudo: SudoMode,
    from: String,
    until: String,
    days: Vec<Weekday>,
    fixed_input: String,
    fixed_times: BTreeSet<Time>,
    selected_fixed: Option<Time>,
    window_start: String,
    window_end: String,
    interval: String,
    age: String,
    pub mode: Mode,
    dependencies: BTreeSet<JobId>,
}

impl Default for Draft {
    fn default() -> Self {
        let standing = Settings::default();
        Self {
            id: None,
            name: String::new(),
            host: String::new(),
            port: "22".into(),
            username: String::new(),
            key_file: String::new(),
            connection: None,
            additional_connections: Vec::new(),
            target_groups: None,
            group_targets_unresolved: false,
            working_directory: String::new(),
            command: text_editor::Content::new(),
            source: Source::Command,
            script_language: Language::default(),
            snippet_id: None,
            script_arguments: text_editor::Content::new(),
            archive_sources: text_editor::Content::new(),
            archive_excludes: text_editor::Content::new(),
            sync_excludes: text_editor::Content::new(),
            sync_approval: None,
            sync_replace_changed: false,
            sync_standing: false,
            sync_standing_days: standing.days.to_string(),
            sync_standing_firings: standing.limits.max_firings.to_string(),
            sync_standing_actions: standing.limits.max_actions.to_string(),
            sync_standing_mib: (standing.limits.max_bytes as f64 / MIB).to_string(),
            rsync_program: String::new(),
            rsync_approval: None,
            agent_goal: String::new(),
            agent_profile: None,
            agent_probe: super::agent::Probe::default(),
            swarm_parallelism: "4".into(),
            agent_approval: None,
            archive_format: ArchiveFormat::default(),
            archive_server_only: false,
            archive_password: None,
            archive_level: "6".into(),
            archive_download: String::new(),
            remote_path: String::new(),
            remote_destination: String::new(),
            remote_mode: String::new(),
            remote_uid: String::new(),
            remote_gid: String::new(),
            remote_delete_confirmation: String::new(),
            recursive_delete: false,
            privileged_copy_tree: false,
            local_file: String::new(),
            privileged_approval: None,
            enabled: true,
            schedule_enabled: false,
            virtual_terminal: TerminalDraft::from_options(None),
            automation_journal: super::automation_journal::Draft::from_options(None),
            desktop_notices: super::notifications::Settings::default(),
            webhook_targets: Vec::new(),
            sudo: SudoMode::Disabled,
            from: String::new(),
            until: String::new(),
            days: Vec::new(),
            fixed_input: "00:00".into(),
            fixed_times: BTreeSet::new(),
            selected_fixed: None,
            window_start: "00:00".into(),
            window_end: "23:59".into(),
            interval: String::new(),
            age: String::new(),
            mode: Mode::Condition,
            dependencies: BTreeSet::new(),
        }
    }
}

impl Draft {
    pub(super) fn from_swarm_handoff(handoff: super::SwarmHandoff) -> Result<Self, String> {
        let super::SwarmHandoff {
            name,
            goal,
            profile,
            targets,
            read_only,
            journal,
        } = handoff;
        let mut targets = targets.into_iter();
        let draft = Self {
            name,
            connection: targets.next(),
            additional_connections: targets.collect(),
            working_directory: "/".into(),
            source: Source::SwarmDiagnostics,
            agent_goal: goal,
            agent_profile: Some(profile),
            agent_probe: if read_only {
                super::agent::Probe::SwarmReadOnly
            } else {
                super::agent::Probe::SwarmIterative
            },
            automation_journal: super::automation_journal::Draft::from_options(journal.as_ref()),
            enabled: false,
            ..Self::default()
        };
        draft.build(JobId::new("swarm-draft").expect("valid draft identifier"))?;
        Ok(draft)
    }

    fn supports_automation_journal(&self) -> bool {
        match self.source {
            Source::AgentDiagnostics => self
                .agent_probe
                .supports_target_journal(super::agent::Kind::Agent),
            Source::SwarmDiagnostics => self
                .agent_probe
                .supports_target_journal(super::agent::Kind::Swarm),
            _ => true,
        }
    }

    pub(super) fn automation_estimate_subject(&self) -> Option<super::automation_journal::Subject> {
        if !self.supports_automation_journal() {
            return None;
        }
        Some(super::automation_journal::Subject {
            source: self.id.as_ref().map(|id| id.as_str().to_owned()),
            schedule: self.enabled.then(|| self.schedule().ok()).flatten(),
            targets: 1 + self.additional_connections.len(),
            mode: self.automation_journal.estimate_mode()?,
        })
    }

    pub(super) fn directory_browser_target(
        &self,
        field: DirectoryField,
    ) -> Option<(&ConnectionRef, &str)> {
        if !self.additional_connections.is_empty() {
            return None;
        }
        let target = self.connection.as_ref()?;
        let path = match field {
            DirectoryField::WorkingDirectory if !self.source.is_remote() => &self.working_directory,
            DirectoryField::RemotePath if self.source.is_remote() => &self.remote_path,
            DirectoryField::RemoteDestination
                if matches!(self.source, Source::RemoteRename | Source::RemoteCopy) =>
            {
                &self.remote_destination
            }
            _ => return None,
        };
        let path = if path.trim().is_empty() {
            "."
        } else if field == DirectoryField::WorkingDirectory || path.ends_with('/') {
            path
        } else {
            parent_path(path).unwrap_or(path)
        };
        Some((target, path))
    }

    fn browse_directory_button(
        &self,
        field: DirectoryField,
        enabled: bool,
    ) -> Element<'_, Message> {
        button("Browse…")
            .style(button::secondary)
            .on_press_maybe(
                (enabled && self.directory_browser_target(field).is_some())
                    .then_some(Message::BrowseDirectory(field)),
            )
            .into()
    }

    pub fn from_job(job: &Job<SshCommand>) -> Self {
        let action = job.action();
        let script = action.script.as_ref();
        let snippet = action.snippet.as_ref();
        let arguments = script
            .map(|script| &script.arguments)
            .or(snippet.map(|link| &link.arguments));
        let mut draft = Self {
            id: Some(job.id().clone()),
            name: job.name().into(),
            host: action.host.clone(),
            port: if action.connection.is_some() {
                "22".into()
            } else {
                action.port.to_string()
            },
            username: action.username.clone(),
            key_file: action.key_file.to_string_lossy().into_owned(),
            connection: action.connection.clone(),
            additional_connections: action.additional_connections.clone(),
            target_groups: action.target_groups.clone(),
            group_targets_unresolved: false,
            working_directory: action.working_directory.clone().unwrap_or_default(),
            command: text_editor::Content::with_text(
                script.map_or(&action.command, |script| &script.body),
            ),
            source: if script.is_some() {
                Source::Script
            } else if snippet.is_some() {
                Source::Snippet
            } else {
                Source::Command
            },
            script_language: script.map_or_else(Language::default, |script| script.language),
            snippet_id: snippet.map(|link| link.id),
            script_arguments: arguments.map_or_else(text_editor::Content::new, |arguments| {
                text_editor::Content::with_text(&arguments.join("\n"))
            }),
            enabled: job.enabled(),
            virtual_terminal: TerminalDraft::from_options(action.virtual_terminal),
            automation_journal: super::automation_journal::Draft::from_options(
                action.automation_journal.as_ref(),
            ),
            desktop_notices: action.desktop_notices,
            webhook_targets: action.webhook_targets.clone(),
            sudo: action.sudo,
            age: optional_text(job.max_age_minutes()),
            mode: job.mode().into(),
            dependencies: job.dependencies().iter().cloned().collect(),
            ..Self::default()
        };
        if let Some(agent) = &action.agent {
            draft.source = match agent.kind {
                super::agent::Kind::Agent => Source::AgentDiagnostics,
                super::agent::Kind::Swarm => Source::SwarmDiagnostics,
            };
            draft.agent_goal = agent.goal.clone();
            draft.agent_profile = Some(agent.profile.clone());
            draft.agent_probe = agent.probe;
            if agent.probe.supports_parallelism() {
                draft.swarm_parallelism = optional_text(agent.swarm_parallelism);
            }
            draft.agent_approval = agent.approval.clone();
        }
        if let Some(remote) = &action.remote {
            match remote {
                RemoteAction::Mkdir { path } => {
                    draft.source = Source::RemoteMkdir;
                    draft.remote_path = path.as_str().into();
                }
                RemoteAction::Rename { from, to } => {
                    draft.source = Source::RemoteRename;
                    draft.remote_path = from.as_str().into();
                    draft.remote_destination = to.as_str().into();
                }
                RemoteAction::Chmod { path, mode } => {
                    draft.source = Source::RemoteChmod;
                    draft.remote_path = path.as_str().into();
                    draft.remote_mode = format!("{mode:04o}");
                }
                RemoteAction::Chown { path, uid, gid } => {
                    draft.source = Source::RemoteChown;
                    draft.remote_path = path.as_str().into();
                    draft.remote_uid = optional_text(*uid);
                    draft.remote_gid = optional_text(*gid);
                }
                RemoteAction::PrivilegedFileAction(metadata) => {
                    draft.remote_path = metadata.path.as_str().into();
                    draft.privileged_approval = metadata.approval.clone();
                    match &metadata.operation {
                        ProtectedOperation::Mkdir => draft.source = Source::RemoteMkdir,
                        ProtectedOperation::Chmod { mode } => {
                            draft.source = Source::RemoteChmod;
                            draft.remote_mode = format!("{mode:04o}");
                        }
                        ProtectedOperation::Chown { uid, gid } => {
                            draft.source = Source::RemoteChown;
                            draft.remote_uid = optional_text(*uid);
                            draft.remote_gid = optional_text(*gid);
                        }
                        ProtectedOperation::Copy { to } => {
                            draft.source = Source::RemoteCopy;
                            draft.remote_destination = to.clone();
                        }
                        ProtectedOperation::CopyTree { to } => {
                            draft.source = Source::RemoteCopy;
                            draft.remote_destination = to.clone();
                            draft.privileged_copy_tree = true;
                        }
                        ProtectedOperation::Rename { to } => {
                            draft.source = Source::RemoteRename;
                            draft.remote_destination = to.clone();
                        }
                        ProtectedOperation::Delete { recursive } => {
                            draft.source = Source::RemoteDelete;
                            draft.recursive_delete = *recursive;
                            draft.remote_delete_confirmation = draft.remote_path.clone();
                        }
                    }
                }
                RemoteAction::CopyRemote { from, to } => {
                    draft.source = Source::RemoteCopy;
                    draft.remote_path = from.as_str().into();
                    draft.remote_destination = to.as_str().into();
                }
                RemoteAction::Archive(Archive {
                    sources,
                    target,
                    format,
                    level,
                    password_credential,
                    excludes,
                    download,
                })
                | RemoteAction::ServerArchive(Archive {
                    sources,
                    target,
                    format,
                    level,
                    password_credential,
                    excludes,
                    download,
                })
                | RemoteAction::PrivilegedArchive(PrivilegedArchive {
                    archive:
                        Archive {
                            sources,
                            target,
                            format,
                            level,
                            password_credential,
                            excludes,
                            download,
                        },
                    ..
                }) => {
                    draft.source = Source::RemoteArchive;
                    draft.archive_server_only = !matches!(remote, RemoteAction::Archive(_));
                    if let RemoteAction::PrivilegedArchive(archive) = remote {
                        draft.privileged_approval.clone_from(&archive.approval);
                    }
                    draft.remote_path = target.as_str().into();
                    draft.archive_format = *format;
                    draft.archive_password.clone_from(password_credential);
                    draft.archive_sources = text_editor::Content::with_text(
                        &sources
                            .iter()
                            .map(|path| path.as_str())
                            .collect::<Vec<_>>()
                            .join("\n"),
                    );
                    draft.archive_excludes = text_editor::Content::with_text(&excludes.join("\n"));
                    draft.archive_level = level.to_string();
                    draft.archive_download = download
                        .as_ref()
                        .map_or_else(String::new, |path| path.to_string_lossy().into_owned());
                }
                RemoteAction::Delete {
                    path,
                    confirmed_path,
                    recursive,
                } => {
                    draft.source = Source::RemoteDelete;
                    draft.remote_path = path.as_str().into();
                    draft.remote_delete_confirmation.clone_from(confirmed_path);
                    draft.recursive_delete = *recursive;
                }
                RemoteAction::Sync(copy)
                | RemoteAction::PrivilegedTreeUpload(copy)
                | RemoteAction::PrivilegedTreeDownload(copy) => {
                    draft.source = match (remote, copy.direction) {
                        (RemoteAction::PrivilegedTreeUpload(_), _) => Source::PrivilegedTreeUpload,
                        (RemoteAction::PrivilegedTreeDownload(_), _) => {
                            Source::PrivilegedTreeDownload
                        }
                        (_, super::sync::Direction::Upload) => Source::SyncUpload,
                        (_, super::sync::Direction::Download) => Source::SyncDownload,
                    };
                    draft.remote_path = copy.remote.as_str().into();
                    draft.local_file = copy.local.to_string_lossy().into_owned();
                    draft.sync_excludes = text_editor::Content::with_text(&copy.excludes);
                    draft.sync_approval = copy.approval.clone();
                    draft.sync_replace_changed = copy.replace_changed;
                    if let (RemoteAction::Sync(_), Some(settings)) = (remote, &copy.standing) {
                        draft.sync_standing = true;
                        draft.sync_standing_days = settings.days.to_string();
                        draft.sync_standing_firings = settings.limits.max_firings.to_string();
                        draft.sync_standing_actions = settings.limits.max_actions.to_string();
                        draft.sync_standing_mib =
                            (settings.limits.max_bytes as f64 / MIB).to_string();
                    }
                }
                RemoteAction::RsyncDownload(transfer) | RemoteAction::RsyncUpload(transfer) => {
                    draft.source = if matches!(remote, RemoteAction::RsyncUpload(_)) {
                        if transfer.directory {
                            Source::RsyncUploadTree
                        } else {
                            Source::RsyncUpload
                        }
                    } else if transfer.directory {
                        Source::RsyncDownloadTree
                    } else {
                        Source::RsyncDownload
                    };
                    draft.remote_path = transfer.remote.as_str().into();
                    draft.local_file = transfer.local.to_string_lossy().into_owned();
                    draft.rsync_program = transfer.program.to_string_lossy().into_owned();
                    draft.rsync_approval = transfer.approval.clone();
                }
                RemoteAction::Privileged(transfer) => {
                    draft.source = match transfer.direction {
                        PrivilegedDirection::Upload => Source::RemoteUpload,
                        PrivilegedDirection::Download => Source::RemoteDownload,
                    };
                    draft.remote_path = transfer.remote.as_str().into();
                    draft.local_file = transfer.local.to_string_lossy().into_owned();
                    draft.privileged_approval = transfer.approval.clone();
                }
                operation @ (RemoteAction::Upload {
                    local,
                    remote,
                    folder,
                }
                | RemoteAction::Download {
                    local,
                    remote,
                    folder,
                }) => {
                    draft.source = match (matches!(operation, RemoteAction::Upload { .. }), *folder)
                    {
                        (true, false) => Source::RemoteUpload,
                        (false, false) => Source::RemoteDownload,
                        (true, true) => Source::RemoteUploadFolder,
                        (false, true) => Source::RemoteDownloadFolder,
                    };
                    draft.remote_path = remote.as_str().into();
                    draft.local_file = local.to_string_lossy().into_owned();
                }
            }
        }
        if let Some(schedule) = job.schedule() {
            draft.schedule_enabled = schedule.enabled();
            let (from, until) = schedule.active_dates();
            draft.from = optional_text(from);
            draft.until = optional_text(until);
            draft.days = schedule.weekdays().collect();
            draft.fixed_times = schedule.fixed_times().collect();
            let (start, end) = schedule.window();
            draft.window_start = time_text(start);
            draft.window_end = time_text(end);
            draft.interval = optional_text(schedule.interval_minutes());
        }
        draft
    }

    pub fn set(&mut self, field: Field, value: String) {
        let target = match field {
            Field::Name => &mut self.name,
            Field::Host => &mut self.host,
            Field::Port => &mut self.port,
            Field::Username => &mut self.username,
            Field::KeyFile => &mut self.key_file,
            Field::WorkingDirectory => &mut self.working_directory,
            Field::RemotePath => &mut self.remote_path,
            Field::RemoteDestination => &mut self.remote_destination,
            Field::RemoteMode => &mut self.remote_mode,
            Field::RemoteUid => &mut self.remote_uid,
            Field::RemoteGid => &mut self.remote_gid,
            Field::ArchiveLevel => &mut self.archive_level,
            Field::ArchiveDownload => &mut self.archive_download,
            Field::RemoteDeleteConfirmation => &mut self.remote_delete_confirmation,
            Field::LocalFile => &mut self.local_file,
            Field::SyncStandingDays => &mut self.sync_standing_days,
            Field::SyncStandingFirings => &mut self.sync_standing_firings,
            Field::SyncStandingActions => &mut self.sync_standing_actions,
            Field::SyncStandingMib => &mut self.sync_standing_mib,
            Field::RsyncProgram => &mut self.rsync_program,
            Field::AgentGoal => &mut self.agent_goal,
            Field::SwarmParallelism => &mut self.swarm_parallelism,
            Field::From => &mut self.from,
            Field::Until => &mut self.until,
            Field::WindowStart => &mut self.window_start,
            Field::WindowEnd => &mut self.window_end,
            Field::Interval => &mut self.interval,
            Field::Age => &mut self.age,
            Field::TerminalColumns => &mut self.virtual_terminal.columns,
            Field::TerminalRows => &mut self.virtual_terminal.rows,
            Field::TerminalInterval => &mut self.virtual_terminal.interval,
            Field::TerminalMaxFrames => &mut self.virtual_terminal.frames,
            Field::TerminalLimit => &mut self.virtual_terminal.limit,
        };
        if value.len() <= MAX_FIELD_BYTES && *target != value {
            *target = value;
            if (self.privileged_file_action()
                && matches!(
                    field,
                    Field::RemotePath
                        | Field::RemoteDestination
                        | Field::RemoteMode
                        | Field::RemoteUid
                        | Field::RemoteGid
                ))
                || (self.privileged_archive()
                    && matches!(field, Field::RemotePath | Field::ArchiveLevel))
            {
                self.privileged_approval = None;
            }
            if matches!(field, Field::RsyncProgram)
                || matches!(
                    self.source,
                    Source::RsyncUpload | Source::RsyncDownloadTree | Source::RsyncUploadTree
                ) && matches!(field, Field::LocalFile | Field::RemotePath)
            {
                self.rsync_approval = None;
            }
            if matches!(
                field,
                Field::Host | Field::Port | Field::Username | Field::KeyFile | Field::RemotePath
            ) {
                self.remote_delete_confirmation.clear();
            }
        }
    }

    pub(super) fn select_agent_profile(&mut self, id: ProfileRef) {
        self.agent_profile = Some(id);
    }

    pub(super) fn rsync_candidate(&self) -> Option<super::rsync::ReviewRequest> {
        if !matches!(
            self.source,
            Source::RsyncDownload
                | Source::RsyncDownloadTree
                | Source::RsyncUpload
                | Source::RsyncUploadTree
        ) || !self.additional_connections.is_empty()
        {
            return None;
        }
        let program = PathBuf::from(&self.rsync_program);
        let upload = if self.source == Source::RsyncUpload {
            let source = PathBuf::from(&self.local_file);
            super::remote::validate_local_path(&source).ok()?;
            let destination = RemotePath::try_from(self.remote_path.clone()).ok()?;
            Some((source, destination))
        } else {
            None
        };
        let directory_upload = if self.source == Source::RsyncUploadTree {
            let source = PathBuf::from(&self.local_file);
            super::remote::validate_local_path(&source).ok()?;
            let destination = RemotePath::try_from(self.remote_path.clone()).ok()?;
            Some((source, destination))
        } else {
            None
        };
        let directory = if self.source == Source::RsyncDownloadTree {
            let destination = PathBuf::from(&self.local_file);
            super::remote::validate_local_path(&destination).ok()?;
            let source = RemotePath::try_from(self.remote_path.clone()).ok()?;
            Some(super::rsync::DirectoryReview {
                source,
                destination,
            })
        } else {
            None
        };
        Some(super::rsync::ReviewRequest {
            program,
            reference: self.connection.clone()?,
            upload,
            directory,
            directory_upload,
        })
    }

    pub(super) fn approve_rsync(&mut self, approval: super::rsync::Approval) -> bool {
        if self
            .rsync_candidate()
            .is_some_and(|request| request.matches(&approval))
        {
            self.rsync_approval = Some(approval);
            true
        } else {
            false
        }
    }

    pub(super) fn edit_sync_excludes(&mut self, action: text_editor::Action) {
        edit_bounded(&mut self.sync_excludes, action, MAX_ARCHIVE_EXCLUDE_TEXT);
    }

    pub(super) fn set_sync_standing(&mut self, enabled: bool) {
        if self.sync_standing != enabled {
            self.sync_approval = None;
        }
        self.sync_standing = enabled;
        if enabled {
            self.sync_replace_changed = false;
        }
    }

    pub(super) fn set_sync_replace_changed(&mut self, enabled: bool) {
        if self.source != Source::SyncDownload || enabled && !cfg!(windows) {
            return;
        }
        if self.sync_replace_changed != enabled {
            self.sync_approval = None;
        }
        self.sync_replace_changed = enabled;
        if enabled {
            self.sync_standing = false;
        }
    }

    fn sync_standing_settings(&self) -> Result<Settings, String> {
        let bytes = self
            .sync_standing_mib
            .trim()
            .parse::<f64>()
            .ok()
            .map(|mib| mib * MIB)
            .filter(|bytes| (1.0..=MAX_BYTES as f64).contains(bytes) && bytes.fract() == 0.0)
            .ok_or_else(|| format!(
                "Enter an SFTP transfer limit up to {} MiB that represents a whole number of bytes",
                MAX_BYTES as f64 / MIB,
            ))?;
        let settings =
            Settings {
                days: self
                    .sync_standing_days
                    .trim()
                    .parse()
                    .map_err(|_| "Enter the number of days the SFTP approval should last")?,
                limits: Limits {
                    max_firings: self
                        .sync_standing_firings
                        .trim()
                        .parse()
                        .map_err(|_| "Enter the maximum number of SFTP firings")?,
                    max_actions: self.sync_standing_actions.trim().parse().map_err(
                        |_| "Enter the maximum number of files and folders per SFTP firing",
                    )?,
                    max_bytes: bytes as u64,
                },
            };
        settings.validate().map_err(|error| error.to_string())?;
        Ok(settings)
    }

    pub fn select_source(&mut self, source: Source) {
        if self.source != source {
            self.remote_delete_confirmation.clear();
            self.recursive_delete = false;
            self.privileged_copy_tree = false;
            self.privileged_approval = None;
            if source != Source::SyncDownload {
                self.sync_replace_changed = false;
            }
            if self.regular_transfer()
                || self.privileged_file_action()
                || self.privileged_archive()
                || source.is_remote()
                || source.is_agent()
            {
                self.sudo = SudoMode::Disabled;
            }
            if source == Source::SwarmDiagnostics {
                self.agent_probe = super::agent::Probe::SwarmReadOnly;
            } else if matches!(
                self.agent_probe,
                super::agent::Probe::SwarmReadOnly | super::agent::Probe::SwarmIterative
            ) {
                self.agent_probe = super::agent::Probe::default();
            }
            self.source = source;
        }
    }

    pub(super) fn regular_transfer(&self) -> bool {
        matches!(self.source, Source::RemoteUpload | Source::RemoteDownload)
    }

    pub(super) fn privileged_file_action(&self) -> bool {
        matches!(
            self.source,
            Source::RemoteMkdir
                | Source::RemoteChmod
                | Source::RemoteChown
                | Source::RemoteCopy
                | Source::RemoteRename
                | Source::RemoteDelete
        )
    }

    pub(super) fn privileged_tree(&self) -> bool {
        matches!(
            self.source,
            Source::PrivilegedTreeUpload | Source::PrivilegedTreeDownload
        )
    }

    pub(super) fn privileged_archive(&self) -> bool {
        self.source == Source::RemoteArchive && self.archive_server_only
    }

    pub(super) fn select_sudo(&mut self, mode: SudoMode) {
        if self.sudo != mode && !(self.privileged_archive() && mode == SudoMode::Vault) {
            self.privileged_approval = None;
            self.sudo = mode;
        }
    }

    pub fn set_recursive_delete(&mut self, recursive: bool) {
        if self.recursive_delete != recursive {
            self.remote_delete_confirmation.clear();
            self.privileged_approval = None;
            self.recursive_delete = recursive;
        }
    }

    pub(super) fn set_privileged_copy_tree(&mut self, enabled: bool) {
        if self.privileged_copy_tree != enabled {
            self.privileged_approval = None;
            self.privileged_copy_tree = enabled;
        }
    }

    pub fn select_connection(&mut self, choice: ConnectionChoice) {
        let connection = choice.reference();
        if self.connection.as_ref() != connection {
            self.remote_delete_confirmation.clear();
            if self.privileged_archive() {
                self.privileged_approval = None;
            }
            self.connection = connection.cloned();
            if let Some(reference) = connection {
                self.additional_connections
                    .retain(|other| other.id != reference.id);
            } else {
                self.additional_connections.clear();
            }
        }
    }

    pub(super) fn add_group_connection(
        &mut self,
        choice: ConnectionChoice,
        store: &ConnectionStore,
    ) -> Result<bool, &'static str> {
        let mut source = self
            .target_groups
            .clone()
            .ok_or("Select a connection group first.")?;
        let reference = choice.reference().ok_or("Choose a saved SSH connection.")?;
        if let Some(selected) = source
            .explicit
            .iter_mut()
            .find(|target| target.id == reference.id)
        {
            selected.clone_from(reference);
        } else {
            source.explicit.push(reference.clone());
        }
        self.apply_target_groups(source, store)
    }

    pub(super) fn remove_group_connection(
        &mut self,
        id: u64,
        store: &ConnectionStore,
    ) -> (bool, Option<&'static str>) {
        let Some(mut source) = self.target_groups.clone() else {
            return (false, None);
        };
        let before = source.explicit.len();
        source.explicit.retain(|target| target.id != id);
        if source.explicit.len() == before {
            return (false, None);
        }
        self.remove_group_selection(source, store)
    }

    pub(super) fn has_connection_groups(&self) -> bool {
        self.target_groups.is_some()
    }

    pub(super) fn require_current_groups(
        &self,
        store: &ConnectionStore,
    ) -> Result<(), &'static str> {
        if let Some(source) = &self.target_groups {
            let resolved = source.resolve(store, self.source == Source::SwarmDiagnostics)?;
            if !resolved
                .iter()
                .eq(self.connection.iter().chain(&self.additional_connections))
            {
                return Err(super::connection::GROUP_TARGETS_CHANGED);
            }
        }
        Ok(())
    }

    pub(super) fn add_connection_group(
        &mut self,
        group: String,
        store: &ConnectionStore,
    ) -> Result<bool, &'static str> {
        let mut source = self.target_groups.clone().unwrap_or_else(|| TargetGroups {
            groups: Vec::new(),
            explicit: self
                .connection
                .iter()
                .chain(&self.additional_connections)
                .cloned()
                .collect(),
        });
        if source.groups.contains(&group) {
            return Ok(false);
        }
        source.groups.push(group);
        self.apply_target_groups(source, store)
    }

    pub(super) fn remove_connection_group(
        &mut self,
        group: &str,
        store: &ConnectionStore,
    ) -> (bool, Option<&'static str>) {
        let Some(mut source) = self.target_groups.clone() else {
            return (false, None);
        };
        let before = source.groups.len();
        source.groups.retain(|path| path != group);
        if source.groups.len() == before {
            return (false, None);
        }
        self.remove_group_selection(source, store)
    }

    pub(super) fn refresh_connection_groups(
        &mut self,
        store: &ConnectionStore,
    ) -> Result<bool, &'static str> {
        let Some(source) = self.target_groups.clone() else {
            return Ok(false);
        };
        self.apply_target_groups(source, store)
    }

    fn apply_target_groups(
        &mut self,
        source: TargetGroups,
        store: &ConnectionStore,
    ) -> Result<bool, &'static str> {
        let targets = self.resolve_group_selection(&source, store)?;
        Ok(self.set_group_selection(source, targets, false))
    }

    fn remove_group_selection(
        &mut self,
        source: TargetGroups,
        store: &ConnectionStore,
    ) -> (bool, Option<&'static str>) {
        match self.resolve_group_selection(&source, store) {
            Ok(targets) => (self.set_group_selection(source, targets, false), None),
            Err(error) => (
                self.set_group_selection(source, Vec::new(), true),
                Some(error),
            ),
        }
    }

    fn resolve_group_selection(
        &self,
        source: &TargetGroups,
        store: &ConnectionStore,
    ) -> Result<Vec<ConnectionRef>, &'static str> {
        if source.groups.is_empty() {
            Ok(source.explicit.clone())
        } else {
            source.resolve(store, self.source == Source::SwarmDiagnostics)
        }
    }

    fn set_group_selection(
        &mut self,
        source: TargetGroups,
        targets: Vec<ConnectionRef>,
        unresolved: bool,
    ) -> bool {
        let source = (!source.groups.is_empty()).then_some(source);
        if self.target_groups == source
            && self.group_targets_unresolved == unresolved
            && self
                .connection
                .iter()
                .chain(&self.additional_connections)
                .eq(&targets)
        {
            return false;
        }
        self.target_groups = source;
        self.group_targets_unresolved = unresolved;
        let mut targets = targets.into_iter();
        self.connection = targets.next();
        self.additional_connections = targets.collect();
        self.remote_delete_confirmation.clear();
        self.privileged_approval = None;
        self.rsync_approval = None;
        self.sync_approval = None;
        self.agent_approval = None;
        true
    }

    fn allows_additional_agent_targets(&self) -> bool {
        self.source != Source::AgentDiagnostics
            || self.agent_probe == super::agent::Probe::PerFiring
    }

    pub fn add_connection(&mut self, choice: ConnectionChoice) -> Result<bool, &'static str> {
        if !self.allows_additional_agent_targets() {
            return Err("Choose per-start proposals before adding more AI Agent targets");
        }
        let (Some(primary), Some(reference)) = (&self.connection, choice.reference()) else {
            return Err("Choose a primary saved SSH connection before adding targets");
        };
        if primary.id == reference.id {
            return Err("This SSH connection is already the primary target");
        }
        if let Some(current) = self
            .additional_connections
            .iter_mut()
            .find(|other| other.id == reference.id)
        {
            if current == reference {
                return Ok(false);
            }
            current.clone_from(reference);
        } else {
            let swarm = self.source == Source::SwarmDiagnostics;
            if self.additional_connections.len() >= super::connection::target_limit(swarm) - 1 {
                return Err(super::connection::target_limit_notice(swarm));
            }
            self.additional_connections.push(reference.clone());
        }
        self.remote_delete_confirmation.clear();
        Ok(true)
    }

    pub fn remove_connection(&mut self, id: u64) -> bool {
        let Some(index) = self
            .additional_connections
            .iter()
            .position(|other| other.id == id)
        else {
            return false;
        };
        self.additional_connections.remove(index);
        self.remote_delete_confirmation.clear();
        true
    }

    pub fn edit_command(&mut self, action: text_editor::Action) {
        edit_bounded(&mut self.command, action, MAX_COMMAND_BYTES);
    }

    pub fn edit_script_arguments(&mut self, action: text_editor::Action) {
        edit_bounded(&mut self.script_arguments, action, MAX_ARGUMENT_TEXT_BYTES);
    }

    pub fn edit_archive_sources(&mut self, action: text_editor::Action) {
        if edit_bounded(&mut self.archive_sources, action, MAX_ARCHIVE_SOURCE_TEXT) {
            self.privileged_approval = None;
        }
    }

    pub fn edit_archive_excludes(&mut self, action: text_editor::Action) {
        if edit_bounded(&mut self.archive_excludes, action, MAX_ARCHIVE_EXCLUDE_TEXT) {
            self.privileged_approval = None;
        }
    }

    pub(super) fn select_archive_format(&mut self, format: ArchiveFormat) -> bool {
        let formats = if self.archive_server_only {
            &ArchiveFormat::SERVER_ONLY[..]
        } else {
            &ArchiveFormat::ALL[..]
        };
        if self.archive_format == format || !formats.contains(&format) {
            return false;
        }
        self.archive_format = format;
        self.privileged_approval = None;
        true
    }

    pub(super) fn set_archive_server_only(&mut self, enabled: bool) -> bool {
        if self.source != Source::RemoteArchive || self.archive_server_only == enabled {
            return false;
        }
        self.archive_server_only = enabled;
        self.sudo = SudoMode::Disabled;
        self.privileged_approval = None;
        let formats = if enabled {
            &ArchiveFormat::SERVER_ONLY[..]
        } else {
            &ArchiveFormat::ALL[..]
        };
        if !formats.contains(&self.archive_format) {
            self.archive_format = ArchiveFormat::Zip;
        }
        true
    }

    pub fn set_dependency(&mut self, id: JobId, checked: bool) {
        if checked {
            self.dependencies.insert(id);
        } else {
            self.dependencies.remove(&id);
        }
    }

    pub fn set_day(&mut self, day: Weekday, checked: bool) {
        self.days.retain(|value| *value != day);
        if checked {
            self.days.push(day);
        }
    }

    pub(super) fn set_all_days(&mut self, checked: bool) -> bool {
        if checked {
            if self.days.len() == DAYS.len() {
                return false;
            }
            self.days = DAYS.map(|(day, _)| day).to_vec();
        } else {
            if self.days.is_empty() {
                return false;
            }
            self.days.clear();
        }
        true
    }

    pub(super) fn set_fixed_input(&mut self, input: String) -> Result<(), String> {
        if input.len() > MAX_FIELD_BYTES {
            self.fixed_input.clear();
            return Err("Enter at most 4096 UTF-8 bytes of fixed times".into());
        }
        self.fixed_input = input;
        Ok(())
    }

    pub(super) fn add_fixed_times(&mut self) -> Result<bool, String> {
        let times = self
            .fixed_input
            .split(',')
            .filter(|time| !time.trim().is_empty())
            .map(parse_time)
            .collect::<Result<Vec<_>, _>>()?;
        if times.is_empty() {
            return Err("Enter a fixed time as HH:MM".into());
        }
        let old_count = self.fixed_times.len();
        self.fixed_times.extend(times);
        Ok(self.fixed_times.len() != old_count)
    }

    pub(super) fn select_fixed_time(&mut self, time: Time) {
        if self.fixed_times.contains(&time) {
            self.selected_fixed = Some(time);
        }
    }

    pub(super) fn remove_fixed_time(&mut self, time: Time) -> bool {
        if self.selected_fixed == Some(time) && self.fixed_times.remove(&time) {
            self.selected_fixed = None;
            true
        } else {
            false
        }
    }

    pub fn build(&self, id: JobId) -> Result<Job<SshCommand>, String> {
        if self.group_targets_unresolved {
            return Err(super::connection::GROUP_TARGETS_CHANGED.into());
        }
        let (host, port, username, key_file) = if self.connection.is_some() {
            (String::new(), 0, String::new(), PathBuf::new())
        } else {
            (
                self.host.trim().into(),
                self.port
                    .trim()
                    .parse()
                    .map_err(|_| "Enter a valid SSH port")?,
                self.username.trim().into(),
                PathBuf::from(self.key_file.trim()),
            )
        };
        let mut action = SshCommand {
            host,
            port,
            username,
            key_file,
            connection: self.connection.clone(),
            additional_connections: self.additional_connections.clone(),
            target_groups: self.target_groups.clone(),
            command: String::new(),
            working_directory: (!self.source.is_remote()
                && !self.working_directory.trim().is_empty())
            .then(|| self.working_directory.clone()),
            script: None,
            snippet: None,
            remote: None,
            agent: None,
            sudo: self.sudo,
            virtual_terminal: if self.source.is_remote() || self.source.is_agent() {
                None
            } else {
                self.virtual_terminal.build()?
            },
            automation_journal: if self.supports_automation_journal() {
                self.automation_journal.build()?
            } else {
                None
            },
            desktop_notices: self.desktop_notices,
            webhook_targets: self.webhook_targets.clone(),
        };
        let remote_path = || self.remote_path.clone().try_into();
        match self.source {
            Source::Command => action.command = self.command.text(),
            Source::Script => {
                action.script = Some(Script::from_lines(
                    self.script_language,
                    self.command.text(),
                    &self.script_arguments.text(),
                ))
            }
            Source::Snippet => {
                action.snippet = Some(SnippetLink {
                    id: self.snippet_id.ok_or("Choose a shared snippet")?,
                    arguments: arguments_from_lines(&self.script_arguments.text()),
                })
            }
            Source::RemoteMkdir => {
                action.remote = Some(RemoteAction::Mkdir {
                    path: remote_path()?,
                })
            }
            Source::RemoteRename => {
                action.remote = Some(RemoteAction::Rename {
                    from: remote_path()?,
                    to: self.remote_destination.clone().try_into()?,
                })
            }
            Source::RemoteChmod => {
                action.remote = Some(RemoteAction::Chmod {
                    path: remote_path()?,
                    mode: mode_from_text(&self.remote_mode)?,
                })
            }
            Source::RemoteChown => {
                action.remote = Some(RemoteAction::Chown {
                    path: remote_path()?,
                    uid: owner_id_from_text(&self.remote_uid)?,
                    gid: owner_id_from_text(&self.remote_gid)?,
                })
            }
            Source::RemoteCopy => {
                action.remote = Some(RemoteAction::CopyRemote {
                    from: remote_path()?,
                    to: self.remote_destination.clone().try_into()?,
                })
            }
            Source::RemoteArchive => {
                let level = if self.archive_format == ArchiveFormat::Tar {
                    6
                } else {
                    self.archive_level
                        .trim()
                        .parse()
                        .map_err(|_| "Enter a valid archive compression level")?
                };
                let archive = Archive {
                    sources: self
                        .archive_sources
                        .text()
                        .lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .map(|line| line.to_owned().try_into())
                        .collect::<Result<_, _>>()?,
                    target: remote_path()?,
                    format: self.archive_format,
                    level,
                    password_credential: self.archive_password.clone().filter(|_| {
                        !self.archive_server_only
                            && self.archive_format == ArchiveFormat::ZipPassword
                    }),
                    excludes: self
                        .archive_excludes
                        .text()
                        .lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .map(str::to_owned)
                        .collect(),
                    download: (!self.archive_server_only && !self.archive_download.is_empty())
                        .then(|| PathBuf::from(&self.archive_download)),
                };
                action.remote = Some(match (self.archive_server_only, self.sudo) {
                    (true, SudoMode::Disabled) => RemoteAction::ServerArchive(archive),
                    (true, _) => RemoteAction::PrivilegedArchive(PrivilegedArchive {
                        archive,
                        approval: self.privileged_approval.clone(),
                    }),
                    (false, _) => RemoteAction::Archive(archive),
                });
            }
            Source::RemoteDelete => {
                let operation = RemoteAction::Delete {
                    path: remote_path()?,
                    confirmed_path: self.remote_delete_confirmation.clone(),
                    recursive: self.recursive_delete,
                };
                operation.validate()?;
                action.remote = Some(operation);
            }
            Source::RemoteUpload | Source::RemoteDownload if self.sudo != SudoMode::Disabled => {
                action.remote = Some(RemoteAction::Privileged(PrivilegedTransfer {
                    direction: if self.source == Source::RemoteUpload {
                        PrivilegedDirection::Upload
                    } else {
                        PrivilegedDirection::Download
                    },
                    local: PathBuf::from(&self.local_file),
                    remote: remote_path()?,
                    approval: self.privileged_approval.clone(),
                }));
            }
            Source::RemoteUpload | Source::RemoteUploadFolder => {
                action.remote = Some(RemoteAction::Upload {
                    local: PathBuf::from(&self.local_file),
                    remote: remote_path()?,
                    folder: self.source == Source::RemoteUploadFolder,
                })
            }
            Source::RemoteDownload | Source::RemoteDownloadFolder => {
                action.remote = Some(RemoteAction::Download {
                    local: PathBuf::from(&self.local_file),
                    remote: remote_path()?,
                    folder: self.source == Source::RemoteDownloadFolder,
                })
            }
            Source::SyncUpload
            | Source::SyncDownload
            | Source::PrivilegedTreeUpload
            | Source::PrivilegedTreeDownload => {
                let copy = super::sync::Copy {
                    local: PathBuf::from(&self.local_file),
                    remote: remote_path()?,
                    direction: if matches!(
                        self.source,
                        Source::SyncDownload | Source::PrivilegedTreeDownload
                    ) {
                        super::sync::Direction::Download
                    } else {
                        super::sync::Direction::Upload
                    },
                    excludes: if self.source == Source::PrivilegedTreeDownload {
                        String::new()
                    } else {
                        self.sync_excludes.text().trim_end_matches('\n').to_owned()
                    },
                    replace_changed: self.source == Source::SyncDownload
                        && self.sync_replace_changed,
                    approval: self.sync_approval.clone(),
                    standing: (self.sync_standing && !self.privileged_tree())
                        .then(|| self.sync_standing_settings())
                        .transpose()?,
                };
                action.remote = Some(match self.source {
                    Source::PrivilegedTreeUpload => RemoteAction::PrivilegedTreeUpload(copy),
                    Source::PrivilegedTreeDownload => RemoteAction::PrivilegedTreeDownload(copy),
                    _ => RemoteAction::Sync(copy),
                });
            }
            Source::RsyncDownload
            | Source::RsyncDownloadTree
            | Source::RsyncUpload
            | Source::RsyncUploadTree => {
                let approval = self.rsync_approval.clone().filter(|approval| {
                    self.rsync_candidate()
                        .is_some_and(|request| request.matches(approval))
                });
                let transfer = super::rsync::Transfer {
                    program: PathBuf::from(&self.rsync_program),
                    local: PathBuf::from(&self.local_file),
                    remote: remote_path()?,
                    directory: matches!(
                        self.source,
                        Source::RsyncDownloadTree | Source::RsyncUploadTree
                    ),
                    approval,
                };
                action.remote = Some(
                    if matches!(self.source, Source::RsyncUpload | Source::RsyncUploadTree) {
                        RemoteAction::RsyncUpload(transfer)
                    } else {
                        RemoteAction::RsyncDownload(transfer)
                    },
                );
            }
            Source::AgentDiagnostics | Source::SwarmDiagnostics => {
                action.agent = Some(super::agent::Action {
                    kind: if self.source == Source::AgentDiagnostics {
                        super::agent::Kind::Agent
                    } else {
                        super::agent::Kind::Swarm
                    },
                    probe: self.agent_probe,
                    profile: self.agent_profile.clone().ok_or("Choose an AI profile.")?,
                    goal: self.agent_goal.clone(),
                    swarm_parallelism: if self.agent_probe.supports_parallelism() {
                        optional_parse(
                            &self.swarm_parallelism,
                            "Choose a Swarm target parallelism from 1 to 16.",
                        )?
                    } else {
                        None
                    },
                    approval: self.agent_approval.clone(),
                });
            }
        }
        if self.sudo != SudoMode::Disabled {
            let metadata = match action.remote.as_ref() {
                Some(RemoteAction::Mkdir { path }) => Some((path, ProtectedOperation::Mkdir)),
                Some(RemoteAction::Chmod { path, mode }) => {
                    Some((path, ProtectedOperation::Chmod { mode: *mode }))
                }
                Some(RemoteAction::Chown { path, uid, gid }) => Some((
                    path,
                    ProtectedOperation::Chown {
                        uid: *uid,
                        gid: *gid,
                    },
                )),
                Some(RemoteAction::CopyRemote { from, to }) => {
                    let to = to.as_str().into();
                    let operation = if self.privileged_copy_tree {
                        ProtectedOperation::CopyTree { to }
                    } else {
                        ProtectedOperation::Copy { to }
                    };
                    Some((from, operation))
                }
                Some(RemoteAction::Rename { from, to }) => Some((
                    from,
                    ProtectedOperation::Rename {
                        to: to.as_str().into(),
                    },
                )),
                Some(RemoteAction::Delete {
                    path, recursive, ..
                }) => Some((
                    path,
                    ProtectedOperation::Delete {
                        recursive: *recursive,
                    },
                )),
                _ => None,
            };
            if let Some((path, operation)) = metadata {
                action.remote = Some(RemoteAction::PrivilegedFileAction(PrivilegedFileAction {
                    path: path.clone(),
                    operation,
                    approval: self.privileged_approval.clone(),
                }));
            }
        }
        let action = action.validated()?;
        let definition = JobDefinition {
            id,
            name: self.name.trim().into(),
            enabled: self.enabled,
            mode: self.mode.into(),
            max_age_minutes: optional_parse(
                &self.age,
                "Enter a positive dependency age in minutes",
            )?,
            dependencies: self.dependencies.iter().cloned().collect(),
            schedule: Some(self.schedule()?),
        };
        let scheduled_copy = match &action.remote {
            Some(RemoteAction::Sync(copy)) => Some((copy, false)),
            Some(
                RemoteAction::PrivilegedTreeUpload(copy)
                | RemoteAction::PrivilegedTreeDownload(copy),
            ) => Some((copy, true)),
            _ => None,
        };
        if let Some((copy, privileged)) = scheduled_copy {
            copy.validate_job_schedule(
                definition.schedule.as_ref(),
                definition.mode,
                &definition.dependencies,
                privileged,
            )?;
        }
        definition
            .with_action(action)
            .map_err(|error| error.to_string())
    }

    fn schedule(&self) -> Result<JobSchedule, String> {
        let from = optional_parse::<Date>(&self.from, "Enter the first active date as YYYY-MM-DD")?;
        let until =
            optional_parse::<Date>(&self.until, "Enter the last active date as YYYY-MM-DD")?;
        let start = parse_time(&self.window_start)?;
        let end = parse_time(&self.window_end)?;
        let interval = optional_parse::<u32>(&self.interval, "Enter an interval in minutes")?
            .and_then(NonZeroU32::new);
        JobSchedule::default()
            .with_enabled(self.schedule_enabled)
            .with_active_dates(from, until)
            .map(|schedule| {
                schedule
                    .with_weekdays(self.days.iter().copied())
                    .with_fixed_times(self.fixed_times.iter().copied())
                    .with_window(start, end)
                    .with_interval_minutes(interval)
            })
            .map_err(|error| error.to_string())
    }

    pub(super) fn job_view<'a>(
        &'a self,
        graph: &'a JobGraph<SshCommand>,
        connections: &'a [ConnectionChoice],
        connection_groups: &'a [String],
        can_browse: bool,
    ) -> Element<'a, Message> {
        let mut dependencies = graph
            .jobs()
            .iter()
            .filter(|job| self.id.as_ref() != Some(job.id()))
            .peekable();
        let mut predecessor_choices = column![text("Predecessors").size(16)].spacing(6);
        if dependencies.peek().is_none() {
            predecessor_choices = predecessor_choices.push(text("No other jobs").size(13));
        }
        for job in dependencies {
            let id = job.id().clone();
            predecessor_choices = predecessor_choices.push(
                checkbox(self.dependencies.contains(&id))
                    .label(job.name())
                    .on_toggle(move |value| Message::Dependency(id.clone(), value)),
            );
        }
        let all_days = checkbox(self.days.len() == DAYS.len())
            .label("All")
            .on_toggle(Message::AllWeekdays);
        let weekdays = DAYS
            .into_iter()
            .fold(row![all_days].spacing(8), |row, (day, label)| {
                row.push(
                    checkbox(self.days.contains(&day))
                        .label(label)
                        .on_toggle(move |checked| Message::Weekday(day, checked)),
                )
            });
        column![
            checkbox(self.enabled).label("Enabled").on_toggle(Message::Enabled),
            form_row("Name", input(Field::Name, "Job name", &self.name)),
            form_row("Connection", self.connection_view(connections)),
            form_row("Connection groups", self.connection_groups_view(connection_groups)),
            (!self.source.is_remote()).then(|| form_row("Working directory", row![
                input(Field::WorkingDirectory, "Remote login directory if blank", &self.working_directory),
                self.browse_directory_button(DirectoryField::WorkingDirectory, can_browse),
            ].spacing(6))),
            text("The SSH host key must already be confirmed. Keep secrets outside the command, script and arguments.").size(12),
            checkbox(self.schedule_enabled)
                .label("Run automatically")
                .on_toggle(Message::ScheduleEnabled),
            form_row("Active from", input(Field::From, "YYYY-MM-DD", &self.from)),
            form_row("Active until", input(Field::Until, "YYYY-MM-DD", &self.until)),
            form_row("Window start", time_entry(Field::WindowStart, &self.window_start)),
            form_row("Window end", time_entry(Field::WindowEnd, &self.window_end)),
            form_row("Interval minutes", input(Field::Interval, "0 or blank for no interval", &self.interval)),
            form_row("Fixed times", self.fixed_time_view()),
            form_row("Weekdays", weekdays.wrap()),
            text("All weekdays are allowed if none are selected. Without times or an interval, the job uses the window start.").size(12),
            text("Dependencies").size(17),
            form_row("Dependency mode", pick_list(Mode::ALL, Some(self.mode), Message::Mode)),
            form_row("Maximum predecessor age", input(Field::Age, "Minutes", &self.age)),
            predecessor_choices,
        ]
        .spacing(8)
        .padding(8)
        .into()
    }

    fn fixed_time_view(&self) -> Element<'_, Message> {
        let controls = row![
            text_input("HH:MM or comma-separated times", &self.fixed_input)
                .width(Length::Fill)
                .on_input(Message::FixedTimeInput)
                .on_submit(Message::AddFixedTimes),
            time_presets(Message::FixedTimeInput),
            button("Add").on_press_maybe(
                (!self.fixed_input.trim().is_empty()).then_some(Message::AddFixedTimes)
            ),
            button("Remove")
                .style(button::secondary)
                .on_press_maybe(self.selected_fixed.map(Message::RemoveFixedTime)),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center);
        let mut content = column![controls].spacing(6);
        if self.fixed_times.is_empty() {
            content = content.push(text("No fixed times").size(13));
        } else {
            let times =
                self.fixed_times
                    .iter()
                    .copied()
                    .fold(column![].spacing(1), |list, time| {
                        let selected = self.selected_fixed == Some(time);
                        list.push(
                            button(text(time_text(time)))
                                .style(move |theme, status| {
                                    if selected {
                                        button::primary(theme, status)
                                    } else {
                                        button::subtle(theme, status)
                                    }
                                })
                                .width(Length::Fill)
                                .on_press(Message::SelectFixedTime(time)),
                        )
                    });
            content = content.push(scrollable(times).height(96).width(Length::Fill));
        }
        content.width(Length::Fill).into()
    }

    pub(super) fn action_view<'a>(
        &'a self,
        snippets: &'a [SnippetChoice],
        archive_passwords: &'a [CredentialChoice],
        webhook_choices: &'a [super::webhooks::Choice],
        agent_profiles: &'a super::agent::ProfileChoices,
        permissions: ActionPermissions,
    ) -> Element<'a, Message> {
        let ActionPermissions {
            browse: can_browse,
            privileged: privileged_allowed,
            automation_journal,
        } = permissions;
        let capture: Element<'_, Message> = if self.regular_transfer() {
            let mut fields = column![].spacing(6);
            if privileged_allowed {
                fields = fields.push(form_row("Privileged staging", self.sudo_view()));
            } else {
                fields =
                    fields.push(text("Application policy forbids privileged staging.").size(12));
                if self.sudo != SudoMode::Disabled {
                    fields = fields.push(
                        button("Disable privileged staging")
                            .on_press(Message::Sudo(SudoMode::Disabled)),
                    );
                }
            }
            if privileged_allowed && self.sudo != SudoMode::Disabled {
                fields = fields
                    .push(text("Privileged staging requires one saved SSH route, verified host keys and a new destination. It supports one regular file on Linux.").size(12))
                    .push(
                        button("Review paths and SSH route…").on_press_maybe(
                            (can_browse && self.connection.is_some() && self.additional_connections.is_empty())
                                .then_some(Message::ReviewPrivileged),
                        ),
                    );
            }
            fields.into()
        } else if self.privileged_tree() {
            if privileged_allowed {
                column![
                    form_row("Privileged staging", self.sudo_view()),
                    text(if self.source == Source::PrivilegedTreeDownload {
                        "Requires a pinned Linux SSH route, a protected server folder, and an absent local destination. Reviews the entire tree up to 128 entries and 1 GiB."
                    } else {
                        "Requires a pinned Linux SSH route and an absent destination folder. Uploads up to 128 reviewed entries and 1 GiB as root with private permissions."
                    }).size(12),
                    button("Review privileged folder transfer…").on_press_maybe(
                        (can_browse && self.sudo != SudoMode::Disabled
                            && self.connection.is_some() && self.additional_connections.is_empty())
                            .then_some(Message::ReviewSync),
                    ),
                ].spacing(6).into()
            } else {
                text("Application policy forbids privileged staging.")
                    .size(12)
                    .into()
            }
        } else if self.privileged_archive() {
            let mut fields = column![].spacing(6);
            if privileged_allowed {
                fields = fields.push(form_row(
                    "Archive execution",
                    pick_list(&SudoMode::ALL[..2], Some(self.sudo), Message::Sudo),
                ));
                if self.sudo == SudoMode::Passwordless {
                    fields = fields
                        .push(
                            text("The protected Linux archive helper must permit the source and destination paths.")
                                .size(12),
                        )
                        .push(
                            button("Review archive and SSH route…").on_press_maybe(
                                (can_browse
                                    && self.connection.is_some()
                                    && self.additional_connections.is_empty())
                                .then_some(Message::ReviewPrivileged),
                            ),
                        );
                }
            } else if self.sudo != SudoMode::Disabled {
                fields = fields
                    .push(text("Application policy forbids privileged archive creation.").size(12))
                    .push(
                        button("Disable privileged execution")
                            .on_press(Message::Sudo(SudoMode::Disabled)),
                    );
            }
            fields.into()
        } else if self.privileged_file_action() {
            let mut fields = column![].spacing(6);
            if privileged_allowed {
                fields = fields.push(form_row("Privileged execution", self.sudo_view()));
            } else if self.sudo != SudoMode::Disabled {
                fields = fields
                    .push(text("Application policy forbids privileged execution.").size(12))
                    .push(
                        button("Disable privileged execution")
                            .on_press(Message::Sudo(SudoMode::Disabled)),
                    );
            }
            if self.sudo == SudoMode::Disabled {
                fields = fields
                    .push(text("This action uses the SFTP subsystem on the server.").size(12));
            } else if privileged_allowed {
                fields = fields
                    .push(
                        text("Requires one pinned Linux SSH route and a protected absolute path.")
                            .size(12),
                    )
                    .push(
                        button("Review action and SSH route…").on_press_maybe(
                            (can_browse
                                && self.connection.is_some()
                                && self.additional_connections.is_empty())
                            .then_some(Message::ReviewPrivileged),
                        ),
                    );
            }
            fields.into()
        } else if self.source.is_remote() {
            text("This action uses the SFTP subsystem on the server.")
                .size(12)
                .into()
        } else if self.source.is_agent() {
            text(match self.agent_probe {
                super::agent::Probe::PerFiring => {
                    "Each firing sends a separate reviewed masked request for each target. Approve the exact SSH commands in the Journal before they run."
                }
                super::agent::Probe::Proposed => {
                    "The AI profile receives the masked request after review. Approve the proposed SSH commands separately for one run."
                }
                super::agent::Probe::SwarmReadOnly => {
                    "Each target runs the fixed reviewed probe and receives a separate masked advisory request. Open the finished run in the Journal to see its saved Swarm report."
                }
                super::agent::Probe::SwarmIterative => {
                    "Review the fixed initial SSH probe and masked model request for each target. Approve later SSH commands in the Journal."
                }
                _ => {
                    "The scheduler runs the fixed reviewed probe. The AI profile receives a masked diagnostic summary request."
                }
            })
            .size(12)
            .into()
        } else {
            column![
                form_row("Run as", self.sudo_view()),
                form_row("Virtual terminal", self.virtual_terminal.view()),
            ]
            .spacing(8)
            .into()
        };
        column![
            self.source_view(snippets, archive_passwords, agent_profiles, can_browse),
            capture,
            self.supports_automation_journal()
                .then(|| self.automation_journal_view(automation_journal)),
            form_row(
                "Outcome notifications",
                self.desktop_notices.view(!self.webhook_targets.is_empty()),
            ),
            form_row("Webhooks", self.webhook_view(webhook_choices)),
        ]
        .spacing(8)
        .padding(8)
        .into()
    }

    fn automation_journal_view(&self, allowed: bool) -> Element<'_, Message> {
        if !allowed {
            return form_row(
                "Automation journal",
                text("Application policy disables automation journal capture.").size(12),
            );
        }
        form_row(
            "Automation journal",
            column![
                self.automation_journal
                    .view(self.source.is_remote())
                    .map(Message::AutomationJournal),
                self.automation_journal
                    .estimate
                    .view(self.automation_estimate_subject().as_ref()),
            ]
            .spacing(6),
        )
    }

    fn webhook_view<'a>(&'a self, choices: &'a [super::webhooks::Choice]) -> Element<'a, Message> {
        use super::webhooks::{Format, MAX_TARGETS};
        use kortty_vault::WebhookFormat;

        let mut targets = column![text(
            "Send selected outcomes by HTTPS. Each vault destination has its own format, enabled state and summary preference."
        ).size(12)]
        .spacing(6);
        for (index, target) in self.webhook_targets.iter().enumerate() {
            let choice = choices.iter().find(|choice| choice.id == target.credential);
            let name = choice.map_or("Locked or missing destination", |choice| {
                choice.label.as_str()
            });
            let settings = choice.map(|choice| choice.settings);
            let format: Element<'_, Message> = match settings {
                Some(settings)
                    if settings.enabled && settings.format == WebhookFormat::JobDefault =>
                {
                    pick_list(Format::ALL, Some(target.format), move |format| {
                        Message::WebhookFormat(index, format)
                    })
                    .into()
                }
                Some(settings) if !settings.enabled => text("Disabled in vault").size(12).into(),
                Some(settings) => text(settings.format.to_string()).size(12).into(),
                None => text(target.format.to_string()).size(12).into(),
            };
            targets = targets.push(
                row![
                    text(name).width(Length::Fill),
                    format,
                    button("Remove").on_press(Message::RemoveWebhook(index)),
                ]
                .spacing(6)
                .align_y(iced::Alignment::Center),
            );
        }
        let available: Vec<_> = choices
            .iter()
            .filter(|choice| {
                choice.settings.enabled
                    && self
                        .webhook_targets
                        .iter()
                        .all(|target| target.credential != choice.id)
            })
            .cloned()
            .collect();
        if self.webhook_targets.len() < MAX_TARGETS && !available.is_empty() {
            targets = targets.push(
                pick_list(
                    available,
                    None::<super::webhooks::Choice>,
                    Message::AddWebhook,
                )
                .placeholder("Add webhook destination"),
            );
        }
        targets
            .push(button("Open credential vault").on_press(Message::OpenCredentialVault))
            .into()
    }

    fn sudo_view(&self) -> Element<'_, Message> {
        let choices = if self.connection.is_some() || self.sudo == SudoMode::Vault {
            &SudoMode::ALL[..]
        } else {
            &SudoMode::ALL[..2]
        };
        let mut fields = column![pick_list(choices, Some(self.sudo), Message::Sudo),].spacing(6);
        if self.sudo == SudoMode::Vault {
            fields = fields.push(
                row![
                    text("Each saved SSH account needs a matching scheduler sudo password. The vault must be unlocked when the job starts.").size(12).width(Length::Fill),
                    button("Open Vault…").on_press(Message::OpenCredentialVault),
                ]
                .spacing(8),
            );
        }
        fields.into()
    }

    fn connection_view<'a>(&'a self, connections: &'a [ConnectionChoice]) -> Element<'a, Message> {
        if let Some(source) = &self.target_groups {
            return self.group_connections_view(connections, source);
        }
        let choice = connections
            .iter()
            .find(|choice| choice.reference() == self.connection.as_ref());
        let mut fields = column![
            pick_list(connections, choice, Message::ConnectionSelected)
                .placeholder("Saved SSH connection changed or unavailable. Choose it again.")
        ]
        .spacing(6);
        if let Some(connection) = &self.connection {
            fields = fields.push(text(connection.target.label()).size(13));
            if let Some(jump) = &connection.jump {
                fields = fields.push(text(format!("Jump host: {}", jump.label())).size(13));
            }
            let additional = connections.get(1..).unwrap_or_default();
            if !additional.is_empty() && self.allows_additional_agent_targets() {
                fields = fields.push(
                    pick_list(
                        additional,
                        None::<&ConnectionChoice>,
                        Message::ConnectionAdded,
                    )
                    .placeholder("Add or reselect a saved target"),
                );
            }
            for (index, target) in self.additional_connections.iter().enumerate() {
                let current = additional
                    .iter()
                    .find(|choice| choice.reference() == Some(target));
                let label = current.map_or_else(
                    || format!("{} (changed or unavailable)", target.target.label()),
                    ToString::to_string,
                );
                let label = if let Some(jump) = &target.jump {
                    format!("{}. {label} via {}", index + 2, jump.label())
                } else {
                    format!("{}. {label}", index + 2)
                };
                fields = fields.push(
                    row![
                        text(label).size(13).width(Length::Fill),
                        button("Remove").on_press(Message::ConnectionRemoved(target.id))
                    ]
                    .spacing(6),
                );
            }
            if !self.additional_connections.is_empty() {
                let help = if self.allows_additional_agent_targets() {
                    "Targets run in the listed order. Virtual terminal recordings and local downloads require one target."
                } else {
                    "Choose per-start proposals for these targets, or remove the additional targets."
                };
                fields = fields.push(text(help).size(12));
            }
            return fields.push(text("Vault credentials require an unlocked vault at run time. Jobs cannot use login prompts, and SSH password jobs cannot record a virtual terminal.").size(12)).into();
        }
        fields
            .push(
                row![
                    input(Field::Host, "Host", &self.host),
                    input(Field::Port, "Port", &self.port).width(70),
                    input(Field::Username, "Username", &self.username),
                ]
                .spacing(8),
            )
            .push(input(
                Field::KeyFile,
                "Absolute SSH private key file",
                &self.key_file,
            ))
            .into()
    }

    fn group_connections_view<'a>(
        &'a self,
        connections: &'a [ConnectionChoice],
        source: &'a TargetGroups,
    ) -> Element<'a, Message> {
        let saved = connections.get(1..).unwrap_or_default();
        let mut fields = column![].spacing(6);
        if !saved.is_empty() {
            fields = fields.push(
                pick_list(saved, None::<&ConnectionChoice>, Message::ConnectionAdded)
                    .placeholder("Add or reselect an individual saved target"),
            );
        }
        for target in &source.explicit {
            let label = saved
                .iter()
                .find(|choice| choice.reference() == Some(target))
                .map_or_else(
                    || format!("{} (changed or unavailable)", target.target.label()),
                    ToString::to_string,
                );
            fields = fields.push(
                row![
                    text(label).width(Length::Fill),
                    button("Remove").on_press(Message::ConnectionRemoved(target.id)),
                ]
                .spacing(6),
            );
        }
        fields = fields.push(
            text(if self.group_targets_unresolved {
                "Resolve or remove the remaining group selections to review the targets."
            } else {
                "Reviewed target order"
            })
            .size(13),
        );
        for (index, target) in self
            .connection
            .iter()
            .chain(&self.additional_connections)
            .enumerate()
        {
            let label = saved
                .iter()
                .find(|choice| choice.reference() == Some(target))
                .map_or_else(
                    || format!("{} (changed or unavailable)", target.target.label()),
                    ToString::to_string,
                );
            fields = fields.push(text(format!("{}. {label}", index + 1)).size(13));
        }
        fields.into()
    }

    fn connection_groups_view<'a>(&'a self, groups: &'a [String]) -> Element<'a, Message> {
        let mut fields = column![].spacing(6);
        if !groups.is_empty() {
            fields = fields.push(
                pick_list(groups, None::<String>, Message::ConnectionGroupAdded)
                    .placeholder("Add a saved connection group"),
            );
        }
        if let Some(source) = &self.target_groups {
            for group in &source.groups {
                fields = fields.push(
                    row![
                        text(group).width(Length::Fill),
                        button("Remove").on_press(Message::ConnectionGroupRemoved(group.clone())),
                    ]
                    .spacing(6),
                );
            }
            fields = fields.push(button("Refresh group targets").on_press(Message::RefreshConnectionGroups))
                .push(text("Subgroups are included. If the targets change, refresh and review the job before it can run.").size(12));
        } else {
            if groups.is_empty() {
                fields = fields.push(text("No saved connection groups").size(12));
            }
            fields = fields.push(
                button("Refresh connection groups").on_press(Message::RefreshConnectionGroups),
            );
        }
        fields.into()
    }

    fn source_view<'a>(
        &'a self,
        snippets: &'a [SnippetChoice],
        archive_passwords: &'a [CredentialChoice],
        agent_profiles: &'a super::agent::ProfileChoices,
        can_browse: bool,
    ) -> Element<'a, Message> {
        let script = self.source == Source::Script;
        let mut editor = column![form_row(
            "Action type",
            pick_list(Source::ALL, Some(self.source), Message::CommandSource).width(Length::Fill),
        )]
        .spacing(8);
        if self.source.is_agent() {
            let kind = if self.source == Source::AgentDiagnostics {
                super::agent::Kind::Agent
            } else {
                super::agent::Kind::Swarm
            };
            let workflows = super::agent::Probe::available(kind);
            let workflow = workflows
                .contains(&self.agent_probe)
                .then_some(self.agent_probe);
            let profile = self
                .agent_profile
                .as_ref()
                .and_then(|reference| agent_profiles.selected(reference));
            let placeholder = if self.agent_profile.is_some() {
                "Selected AI profile is unavailable"
            } else {
                "Choose an AI profile"
            };
            editor = editor
                .push(form_row(
                    "AI profile",
                    pick_list(
                        agent_profiles.entries.as_slice(),
                        profile,
                        Message::AgentProfile,
                    )
                    .placeholder(placeholder),
                ))
                .push(form_row(
                    "Workflow",
                    pick_list(workflows, workflow, Message::AgentProbe),
                ));
            if workflow.is_some_and(super::agent::Probe::supports_parallelism) {
                let parallelism = self.swarm_parallelism.parse::<u8>().unwrap_or(1);
                editor = editor.push(form_row(
                    "Target parallelism",
                    pick_list(SWARM_PARALLELISM_CHOICES, Some(parallelism), |value| {
                        Message::Edit(Field::SwarmParallelism, value.to_string())
                    }),
                ));
            }
            return editor
                .push(form_row("Question", input(Field::AgentGoal, "What should this workflow address?", &self.agent_goal)))
                .push(text("Set an absolute SSH working directory in the Job tab. Review the selected profile and host keys before running.").size(12))
                .push(button("Review AI workflow…").on_press(Message::ReviewAgent))
                .into();
        }
        if script {
            editor = editor.push(form_row(
                "Language",
                pick_list(
                    Language::ALL,
                    Some(self.script_language),
                    Message::ScriptLanguage,
                ),
            ));
        }
        if self.source.is_remote() {
            editor = editor.push(form_row(
                "Remote path",
                row![
                    input(
                        Field::RemotePath,
                        if self.source == Source::RemoteArchive {
                            "Absolute new archive path on the SSH server"
                        } else {
                            "Absolute path on the SSH server"
                        },
                        &self.remote_path,
                    ),
                    self.browse_directory_button(DirectoryField::RemotePath, can_browse),
                ]
                .spacing(6),
            ));
            let destination = || {
                form_row(
                    "Remote destination",
                    row![
                        input(
                            Field::RemoteDestination,
                            "Absolute new path on the SSH server",
                            &self.remote_destination
                        ),
                        self.browse_directory_button(DirectoryField::RemoteDestination, can_browse),
                    ]
                    .spacing(6),
                )
            };
            return match self.source {
                Source::RemoteRename => editor.push(destination()),
                Source::RemoteChmod => editor.push(form_row("Permissions", input(
                    Field::RemoteMode,
                    "Octal permissions (e.g. 0755)",
                    &self.remote_mode,
                ))),
                Source::RemoteChown => editor
                    .push(form_row("Owner", input(Field::RemoteUid, "Numeric UID (optional)", &self.remote_uid)))
                    .push(form_row("Group", input(Field::RemoteGid, "Numeric GID (optional)", &self.remote_gid)))
                    .push(text(if self.sudo == SudoMode::Disabled {
                        "Set at least one numeric ID. The server must support lsetstat."
                    } else {
                        "Set at least one numeric ID."
                    }).size(12)),
                Source::RemoteCopy => {
                    let mut copy = editor.push(destination());
                    if self.sudo != SudoMode::Disabled {
                        copy = copy.push(
                            checkbox(self.privileged_copy_tree)
                                .label("Copy a protected folder recursively")
                                .on_toggle(Message::PrivilegedCopyTree),
                        );
                    }
                    copy.push(text(if self.sudo == SudoMode::Disabled {
                        "Copies a file or up to 4,096 folder entries, with up to 1 GiB of file data. Requires copy-data and hard-link support. An interrupted copy may leave a partial folder."
                    } else if self.privileged_copy_tree {
                        "Copies up to 128 protected entries and 1 GiB of file data to a new folder. Mounts and links are refused."
                    } else {
                        "Copies up to 1 GiB from a protected regular file to a new root-owned file with mode 0600."
                    }).size(12))
                }
                Source::RemoteArchive => {
                    let formats: &[ArchiveFormat] = if self.archive_server_only { &ArchiveFormat::SERVER_ONLY } else { &ArchiveFormat::ALL };
                    let mut archive = editor
                        .push(checkbox(self.archive_server_only).label("Create entirely on the server").on_toggle(Message::ArchiveServerOnly))
                        .push(form_row("Archive format", pick_list(formats, Some(self.archive_format), Message::ArchiveFormat)))
                        .push(form_row("Archive sources", text_editor(&self.archive_sources).placeholder("One absolute source file or folder path per line").height(Length::Fixed(110.0)).on_action(Message::EditArchiveSources)))
                        .push(form_row("Archive excludes", text_editor(&self.archive_excludes).placeholder("One optional exclude glob per line, e.g. *.log").height(Length::Fixed(80.0)).on_action(Message::EditArchiveExcludes)));
                    let compression = match self.archive_format {
                        ArchiveFormat::Zip | ArchiveFormat::ZipPassword => Some("ZIP compression level 0–9"),
                        ArchiveFormat::SevenZip => Some("7z compression level 0–9"),
                        ArchiveFormat::Tar | ArchiveFormat::SevenZipPassword => None,
                        ArchiveFormat::TarBz2 => Some("bzip2 compression level 1–9"),
                    };
                    if let Some(label) = compression {
                        archive = archive.push(form_row("Compression", input(Field::ArchiveLevel, label, &self.archive_level)));
                    }
                    if self.archive_format == ArchiveFormat::ZipPassword {
                        let choice = archive_passwords.iter().find(|choice| {
                            Some(choice.id.as_str()) == self.archive_password.as_deref()
                        });
                        archive = archive
                            .push(form_row("Archive password", row![
                                pick_list(archive_passwords, choice, Message::ArchivePassword)
                                    .placeholder(if self.archive_password.is_some() {
                                        "Saved password reference"
                                    } else {
                                        "Choose a ZIP archive password"
                                    }),
                                button("Open Vault…").on_press(Message::OpenCredentialVault),
                            ].spacing(8)))
                            .push(text("The vault must be unlocked when the job starts. AES-256 protects file contents. File names remain visible.").size(12));
                    }
                    if !self.archive_server_only {
                        archive = archive.push(form_row("Local archive path", input(Field::ArchiveDownload, "Optional absolute local archive path", &self.archive_download)));
                    }
                    archive
                        .push(text("Glob paths start with the source name. A leading / matches the absolute remote path. TAR also checks from each directory boundary.").size(12))
                        .push(text(if self.privileged_archive() && self.sudo == SudoMode::Passwordless {
                            "Creates a new root-owned archive with private permissions using one saved Linux SSH connection and verified host keys."
                        } else if self.archive_server_only {
                            "Creates a private archive at a new path using the server archive helper and one saved SSH connection with verified host keys."
                        } else {
                            "Creates a private archive at a new path. Requires SFTP file-transfer permission and server hard-link support."
                        }).size(12))
                        .push(text("Limits are 16 sources, 256 MiB input and a 64 MiB archive.").size(12))
                }
                Source::RemoteDelete => editor
                    .push(checkbox(self.recursive_delete).label(if self.sudo == SudoMode::Disabled {
                        "Also delete directory contents (up to 4096 entries)"
                    } else {
                        "Also delete protected directory contents (up to 128 entries)"
                    }).on_toggle(Message::RecursiveDelete))
                    .push(form_row("Confirmation", input(Field::RemoteDeleteConfirmation, "Repeat the exact remote path to confirm", &self.remote_delete_confirmation)))
                    .push(text("Every run permanently deletes the configured path. A failed or cancelled recursive run may leave a partially deleted tree.").size(12)),
                Source::RemoteUpload | Source::RemoteDownload => editor
                    .push(form_row("Local path", input(Field::LocalFile, "Absolute local file path", &self.local_file)))
                    .push(text(if self.sudo == SudoMode::Disabled {
                        "Transfers require a new destination file and support up to 1 GiB per run."
                    } else {
                        "Privileged transfers require a new destination file and a separate saved approval."
                    }).size(12)),
                Source::RemoteUploadFolder => editor
                    .push(form_row("Local path", input(Field::LocalFile, "Absolute local source folder", &self.local_file)))
                    .push(text("Creates a new private server folder and skips links and special files. Requires hard-link support. An interrupted upload can leave a private partial folder.").size(12)),
                Source::RemoteDownloadFolder => editor
                    .push(form_row("Local path", input(Field::LocalFile, "Absolute new local folder", &self.local_file)))
                    .push(text("Creates the local folder after the download completes. Links and special files are skipped.").size(12)),
                Source::PrivilegedTreeDownload => editor
                    .push(form_row("Local destination", input(Field::LocalFile, "Absolute new local folder", &self.local_file)))
                    .push(text("Save one approval for Run Now or one scheduled date and time.").size(12)),
                Source::SyncUpload | Source::SyncDownload | Source::PrivilegedTreeUpload => {
                    let editor = editor
                        .push(form_row("Local folder", input(Field::LocalFile, "Absolute existing local folder", &self.local_file)))
                        .push(form_row("Excludes", text_editor(&self.sync_excludes)
                            .placeholder("One optional relative glob per line")
                            .height(Length::Fixed(80.0))
                            .on_action(Message::EditSyncExcludes)));
                    if self.privileged_tree() {
                        editor.push(text("Save one approval for Run Now or one scheduled date and time.").size(12))
                    } else {
                        let editor = if self.source == Source::SyncDownload
                            && (cfg!(windows) || self.sync_replace_changed)
                        {
                            editor.push(checkbox(self.sync_replace_changed)
                                .label("Replace changed local files after review")
                                .on_toggle(Message::SyncReplaceChanged))
                        } else {
                            editor
                        };
                        let editor = editor.push(
                            checkbox(self.sync_standing)
                                .label("Allow recurring scheduled copies on this device")
                                .on_toggle(Message::SyncStanding),
                        );
                        let editor = if self.sync_standing {
                            editor
                                .push(form_row("Approval duration (days)", input(Field::SyncStandingDays, "Days", &self.sync_standing_days)))
                                .push(form_row("Maximum firings", input(Field::SyncStandingFirings, "Firings", &self.sync_standing_firings)))
                                .push(form_row("Files and folders per firing", input(Field::SyncStandingActions, "Maximum count", &self.sync_standing_actions)))
                                .push(form_row("MiB per firing", input(Field::SyncStandingMib, "Maximum MiB", &self.sync_standing_mib)))
                                .push(text("The saved calendar can copy newly missing files and folders within these limits. Each authorized firing counts toward the total.").size(12))
                        } else if self.sync_replace_changed {
                            editor.push(text(if cfg!(windows) {
                                "Compare regular file contents. Each replaced file retains its original in a private sibling folder. Save one approval for one run."
                            } else {
                                "Changed-file replacement requires a Windows destination."
                            }).size(12))
                        } else {
                            editor.push(text("Copy only the reviewed missing files and folders. Save one approval for Run Now or one scheduled date and time.").size(12))
                        };
                        editor.push(button("Compare and review…").on_press(Message::ReviewSync))
                    }
                }
                Source::RsyncDownload | Source::RsyncDownloadTree | Source::RsyncUpload | Source::RsyncUploadTree => editor
                    .push(if self.source == Source::RsyncUpload {
                        form_row("Local source", input(Field::LocalFile, "Absolute regular local file path", &self.local_file))
                    } else if self.source == Source::RsyncUploadTree {
                        form_row("Local source", input(Field::LocalFile, "Absolute local folder path", &self.local_file))
                    } else if self.source == Source::RsyncDownloadTree {
                        form_row("Local destination", input(Field::LocalFile, "Absolute new local folder path", &self.local_file))
                    } else {
                        form_row("Local destination", input(Field::LocalFile, "Absolute new local file path", &self.local_file))
                    })
                    .push(form_row("Rsync executable", input(Field::RsyncProgram, "Absolute path to the installed native rsync executable", &self.rsync_program)))
                    .push(text(match (cfg!(unix), self.source) {
                        (true, Source::RsyncDownloadTree) => "One server directory with only ordinary files and directories, up to 4,096 entries and 256 MiB. Requires native rsync 3.4.1 or later in the rsync 3 series and one pinned saved SSH route.",
                        (true, Source::RsyncUploadTree) => "One reviewed local directory, up to 4,096 entries and 256 MiB, uploaded to a new server directory. Requires native rsync 3.4.1 or later, one pinned saved SSH route, and the current korTTY helper on Linux or macOS.",
                        (true, _) => "One regular file, up to 256 MiB, and a new destination. Requires native rsync 3.4.1 or later in the rsync 3 series and one pinned saved SSH route.",
                        (false, _) => "Scheduled rsync transfers require a Unix rsync executable.",
                    }).size(12))
                    .push(button("Review rsync transfer…").on_press_maybe(cfg!(unix).then_some(Message::ReviewRsyncProgram))),
                _ => editor,
            }
            .into();
        }
        if self.source == Source::Snippet {
            let choice = snippets
                .iter()
                .find(|choice| Some(choice.id) == self.snippet_id);
            editor = editor.push(form_row(
                "Snippet",
                pick_list(snippets, choice, Message::SnippetSelected).placeholder(
                    if self.snippet_id.is_some() && choice.is_none() {
                        "Linked snippet unavailable"
                    } else {
                        "Choose a saved Bash, shell, Python, Perl or Ruby snippet"
                    },
                ),
            ));
            editor = editor
                .push(text("Runs use the latest saved version of the linked snippet.").size(12));
        } else {
            editor = editor.push(form_row(
                if script { "Script" } else { "Command" },
                text_editor(&self.command)
                    .font(iced::Font::MONOSPACE)
                    .height(120)
                    .placeholder(if script {
                        "Script to run on the SSH server"
                    } else {
                        "Command to run on the SSH server"
                    })
                    .on_action(Message::EditCommand),
            ));
        }
        if self.source != Source::Command {
            editor = editor.push(form_row(
                "Arguments",
                text_editor(&self.script_arguments)
                    .font(iced::Font::MONOSPACE)
                    .height(78)
                    .placeholder("Optional arguments, one per line")
                    .on_action(Message::EditScriptArguments),
            )).push(text("Scripts use a POSIX login shell and the selected interpreter on the server.").size(12));
        }
        editor.into()
    }
}

fn edit_bounded(
    content: &mut text_editor::Content,
    action: text_editor::Action,
    limit: usize,
) -> bool {
    if matches!(&action, text_editor::Action::Edit(text_editor::Edit::Paste(text)) if text.len() > limit)
    {
        return false;
    }
    if !action.is_edit() {
        content.perform(action);
        return false;
    }
    let old = content.text();
    let cursor = content.cursor();
    content.perform(action);
    let new = content.text();
    if new.len() > limit {
        *content = text_editor::Content::with_text(&old);
        content.move_to(cursor);
        false
    } else {
        old != new
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Source {
    Command,
    Script,
    Snippet,
    RemoteMkdir,
    RemoteRename,
    RemoteChmod,
    RemoteChown,
    RemoteCopy,
    RemoteArchive,
    RemoteDelete,
    RemoteUpload,
    RemoteDownload,
    RemoteUploadFolder,
    RemoteDownloadFolder,
    SyncUpload,
    SyncDownload,
    PrivilegedTreeUpload,
    PrivilegedTreeDownload,
    RsyncDownload,
    RsyncDownloadTree,
    RsyncUpload,
    RsyncUploadTree,
    AgentDiagnostics,
    SwarmDiagnostics,
}

impl Source {
    const ALL: [Self; 24] = [
        Self::Command,
        Self::Script,
        Self::Snippet,
        Self::RemoteMkdir,
        Self::RemoteRename,
        Self::RemoteChmod,
        Self::RemoteChown,
        Self::RemoteCopy,
        Self::RemoteArchive,
        Self::RemoteDelete,
        Self::RemoteUpload,
        Self::RemoteDownload,
        Self::RemoteUploadFolder,
        Self::RemoteDownloadFolder,
        Self::SyncUpload,
        Self::SyncDownload,
        Self::PrivilegedTreeUpload,
        Self::PrivilegedTreeDownload,
        Self::RsyncDownload,
        Self::RsyncDownloadTree,
        Self::RsyncUpload,
        Self::RsyncUploadTree,
        Self::AgentDiagnostics,
        Self::SwarmDiagnostics,
    ];

    fn is_remote(self) -> bool {
        matches!(
            self,
            Self::RemoteMkdir
                | Self::RemoteRename
                | Self::RemoteChmod
                | Self::RemoteChown
                | Self::RemoteCopy
                | Self::RemoteArchive
                | Self::RemoteDelete
                | Self::RemoteUpload
                | Self::RemoteDownload
                | Self::RemoteUploadFolder
                | Self::RemoteDownloadFolder
                | Self::SyncUpload
                | Self::SyncDownload
                | Self::PrivilegedTreeUpload
                | Self::PrivilegedTreeDownload
                | Self::RsyncDownload
                | Self::RsyncDownloadTree
                | Self::RsyncUpload
                | Self::RsyncUploadTree
        )
    }

    fn is_agent(self) -> bool {
        matches!(self, Self::AgentDiagnostics | Self::SwarmDiagnostics)
    }
}

impl std::fmt::Display for Source {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Command => "Command",
            Self::Script => "Script",
            Self::Snippet => "Shared snippet",
            Self::RemoteMkdir => "SFTP · Create directory",
            Self::RemoteRename => "SFTP · Rename or move",
            Self::RemoteChmod => "SFTP · Change permissions",
            Self::RemoteChown => "SFTP · Change ownership",
            Self::RemoteCopy => "SFTP · Copy on server",
            Self::RemoteArchive => "SFTP · Create archive",
            Self::RemoteDelete => "SFTP · Permanently delete",
            Self::RemoteUpload => "SFTP · Upload file",
            Self::RemoteDownload => "SFTP · Download file",
            Self::RemoteUploadFolder => "SFTP · Upload folder",
            Self::RemoteDownloadFolder => "SFTP · Download folder",
            Self::SyncUpload => "SFTP · Review folder upload",
            Self::SyncDownload => "SFTP · Review folder download",
            Self::PrivilegedTreeUpload => "SSH · Review privileged folder upload",
            Self::PrivilegedTreeDownload => "SSH · Review privileged folder download",
            Self::RsyncDownload => "rsync · Download new file",
            Self::RsyncDownloadTree => "rsync · Download new folder",
            Self::RsyncUpload => "rsync · Upload new file",
            Self::RsyncUploadTree => "rsync · Upload new folder",
            Self::AgentDiagnostics => "AI Agent · Scheduled SSH workflow",
            Self::SwarmDiagnostics => "AI Swarm · Scheduled SSH workflow",
        })
    }
}

pub(super) struct TerminalDraft {
    pub enabled: bool,
    columns: String,
    rows: String,
    interval: String,
    frames: String,
    limit: String,
    pub capture_changes: bool,
    pub limit_is_success: bool,
}

impl TerminalDraft {
    fn from_options(options: Option<TerminalOptions>) -> Self {
        let enabled = options.is_some();
        let options = options.unwrap_or_default();
        Self {
            enabled,
            columns: options.columns.to_string(),
            rows: options.rows.to_string(),
            interval: options.interval_seconds.to_string(),
            frames: options.max_frames.to_string(),
            limit: options.limit_seconds.to_string(),
            capture_changes: options.capture_changes,
            limit_is_success: options.limit_is_success,
        }
    }

    fn build(&self) -> Result<Option<TerminalOptions>, String> {
        if !self.enabled {
            return Ok(None);
        }
        TerminalOptions {
            columns: parse_count(&self.columns, "columns")?,
            rows: parse_count(&self.rows, "rows")?,
            interval_seconds: parse_count(&self.interval, "seconds between frames")?,
            capture_changes: self.capture_changes,
            max_frames: parse_count(&self.frames, "maximum frames")?,
            limit_seconds: parse_count(&self.limit, "runtime limit in seconds")?,
            limit_is_success: self.limit_is_success,
        }
        .validate()
        .map(Some)
    }

    fn view(&self) -> Element<'_, Message> {
        let mut settings = column![
            checkbox(self.enabled)
                .label("Use virtual terminal and save screen frames")
                .on_toggle(Message::VirtualTerminal)
        ]
        .spacing(6);
        if self.enabled {
            settings = settings
                .push(row![
                    terminal_field(Field::TerminalColumns, "Columns", &self.columns),
                    terminal_field(Field::TerminalRows, "Rows", &self.rows),
                    terminal_field(Field::TerminalInterval, "Frame interval (seconds)", &self.interval),
                    terminal_field(Field::TerminalMaxFrames, "Maximum frames", &self.frames),
                    terminal_field(Field::TerminalLimit, "Runtime limit (seconds)", &self.limit),
                ].spacing(8).wrap())
                .push(checkbox(self.capture_changes)
                    .label("Capture changed screens")
                    .on_toggle(Message::CaptureChanges))
                .push(checkbox(self.limit_is_success)
                    .label("Count reaching the runtime limit as success")
                    .on_toggle(Message::LimitIsSuccess))
                .push(text("An interval of zero disables periodic frames. The runtime limit is capped at one hour, including when set to zero. The journal stores visible output, which can contain sensitive data.").size(12));
        }
        settings.into()
    }
}

fn parse_count<T: std::str::FromStr>(value: &str, label: &str) -> Result<T, String> {
    value
        .trim()
        .parse()
        .map_err(|_| format!("Virtual terminal: enter a number for {label}"))
}

fn terminal_field<'a>(field: Field, label: &'static str, value: &'a str) -> Element<'a, Message> {
    column![text(label).size(12), input(field, label, value)]
        .width(145)
        .spacing(3)
        .into()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    Condition,
    Chain,
}

impl Mode {
    const ALL: [Self; 2] = [Self::Condition, Self::Chain];
}

impl std::fmt::Display for Mode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Condition => "Condition",
            Self::Chain => "Chain",
        })
    }
}

impl From<DependencyMode> for Mode {
    fn from(mode: DependencyMode) -> Self {
        match mode {
            DependencyMode::Gate => Self::Condition,
            DependencyMode::Chain => Self::Chain,
        }
    }
}

impl From<Mode> for DependencyMode {
    fn from(mode: Mode) -> Self {
        match mode {
            Mode::Condition => Self::Gate,
            Mode::Chain => Self::Chain,
        }
    }
}

fn form_row<'a>(
    label: &'static str,
    control: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    row![
        iced::widget::container(text(label).size(14))
            .padding([5, 0])
            .width(170),
        control.into(),
    ]
    .spacing(10)
    .width(Length::Fill)
    .into()
}

fn input<'a>(
    field: Field,
    placeholder: &str,
    value: &'a str,
) -> iced::widget::TextInput<'a, Message> {
    text_input(placeholder, value)
        .width(Length::Fill)
        .on_input(move |value| Message::Edit(field, value))
}

fn time_entry(field: Field, value: &str) -> Element<'_, Message> {
    row![
        input(field, "HH:MM", value),
        time_presets(move |value| Message::Edit(field, value)),
    ]
    .spacing(8)
    .align_y(iced::Alignment::Center)
    .into()
}

fn time_presets<'a>(select: impl Fn(String) -> Message + 'a) -> Element<'a, Message> {
    static VALUES: OnceLock<Vec<String>> = OnceLock::new();
    let values = VALUES.get_or_init(|| {
        (0..24 * 60)
            .step_by(5)
            .chain(std::iter::once(23 * 60 + 59))
            .map(|minute| format!("{:02}:{:02}", minute / 60, minute % 60))
            .collect()
    });
    pick_list(values.as_slice(), None::<String>, select)
        .placeholder("Presets")
        .into()
}

fn optional_parse<T: std::str::FromStr>(value: &str, error: &str) -> Result<Option<T>, String> {
    let value = value.trim();
    (!value.is_empty())
        .then(|| value.parse().map_err(|_| error.to_owned()))
        .transpose()
}

fn optional_text(value: Option<impl ToString>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

fn parse_time(value: &str) -> Result<Time, String> {
    let value = value.trim();
    if value.len() == 5
        && value.as_bytes().get(2) == Some(&b':')
        && let Ok(time) = value.parse()
    {
        return Ok(time);
    }
    Err("Enter schedule times as HH:MM".into())
}

fn time_text(time: Time) -> String {
    format!("{:02}:{:02}", time.hour(), time.minute())
}
