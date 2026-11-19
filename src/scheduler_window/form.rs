use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use iced::widget::{
    button, checkbox, column, pick_list, row, scrollable, text, text_editor, text_input,
};
use iced::{Element, Length};
use kortty::ssh_runtime::account_lookup::Kind as AccountKind;
use kortty_jobs::{
    Date, DependencyMode, Job, JobDefinition, JobGraph, JobId, JobSchedule, Time, Weekday,
};
use kortty_server_archive::protocol::ArchiveMemberLayout;

use crate::ai_manager::ProfileRef;
use crate::connection_store::Store as ConnectionStore;
use crate::connection_window::CredentialChoice;
use crate::job_executor::virtual_terminal::Options as TerminalOptions;
use crate::sftp_window::privileged_file::ProtectedOperation;
use crate::snippets::SnippetId;

use super::ownership;
use super::privileged::{
    PrivilegedArchive, PrivilegedDirection, PrivilegedFileAction, PrivilegedTransfer,
};
use super::remote::{
    Archive, ArchiveFormat, MAX_ARCHIVE_EXCLUDE_TEXT, MAX_ARCHIVE_SOURCE_TEXT, RemoteAction,
    RemotePath, SymbolicMode, mode_from_text, owner_id_from_text,
};
use super::script::{
    Language, MAX_ARGUMENT_TEXT_BYTES, MAX_COMMAND_BYTES, Script, SnippetLink, arguments_from_lines,
};
use super::sync::standing::{Limits, MAX_BYTES, MAX_REPLACEMENT_BYTES, Settings};
use super::working_directory::parent_path;
use super::{
    ConnectionChoice, ConnectionRef, DirectoryField, ImportedAiAction, JournalDetail, Message,
    SnippetChoice, SshCommand, SudoMode, TargetGroups, imported_archive, imported_privileged,
    imported_transfer,
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

fn java_archive_exclude_help(format: ArchiveFormat) -> &'static str {
    if format.is_zip() {
        "Relative globs match complete ZIP entry paths such as etc/hosts. A relative match for a directory excludes only its own entry. A leading / matches the absolute server path and excludes a matching directory tree."
    } else {
        "TAR checks relative globs against the complete archive path and at directory boundaries. A leading / matches the absolute server path. Matching directories are excluded with their contents."
    }
}

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
    ArchiveUid,
    ArchiveGid,
    ArchiveMode,
    RemoteDeleteConfirmation,
    LocalFile,
    SyncStandingDays,
    SyncStandingFirings,
    SyncStandingActions,
    SyncStandingMib,
    RsyncProgram,
    RsyncStandingDays,
    RsyncStandingFirings,
    RsyncStandingEntries,
    RsyncStandingMib,
    RsyncStandingDeletes,
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
    pub(super) journal_output: bool,
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
    rsync_existing: Option<super::rsync::ExistingDestination>,
    rsync_allow_vacant_directory_download: bool,
    rsync_allow_vacant_file_download: bool,
    rsync_allow_vacant_server_file_upload: bool,
    rsync_allow_vacant_server_tree_upload: bool,
    rsync_mixed: bool,
    rsync_elevation: super::rsync::Elevation,
    rsync_sources: text_editor::Content,
    rsync_destinations: BTreeMap<u64, String>,
    rsync_approval: Option<super::rsync::Reviewed>,
    rsync_standing: bool,
    rsync_standing_id: Option<String>,
    rsync_standing_days: String,
    rsync_standing_firings: String,
    rsync_standing_entries: String,
    rsync_standing_mib: String,
    rsync_standing_deletes: String,
    rsync_import: Option<Box<imported_transfer::RsyncDraft>>,
    agent_goal: text_editor::Content,
    agent_profile: Option<ProfileRef>,
    imported_ai: Option<Box<ImportedAiAction>>,
    imported_archive: Option<Box<imported_archive::Draft>>,
    imported_transfer: Option<Box<imported_transfer::Draft>>,
    imported_privileged: Option<Box<imported_privileged::Draft>>,
    imported_sudo_mode: Option<SudoMode>,
    pub(super) agent_probe: super::agent::Probe,
    swarm_parallelism: String,
    agent_approval: Option<Box<super::agent::Approval>>,
    pub(super) archive_format: ArchiveFormat,
    archive_member_layout: ArchiveMemberLayout,
    pub(super) archive_server_only: bool,
    archive_reuse_output: bool,
    pub(super) archive_password: Option<String>,
    archive_level: String,
    archive_download: String,
    archive_uid: String,
    archive_gid: String,
    archive_mode: String,
    reviewed_archive_uid: Option<Arc<ownership::Route>>,
    reviewed_archive_gid: Option<Arc<ownership::Route>>,
    remote_path: String,
    remote_destination: String,
    remote_mode: String,
    remote_uid: String,
    remote_gid: String,
    reviewed_uid: Option<Arc<ownership::Route>>,
    reviewed_gid: Option<Arc<ownership::Route>>,
    remote_delete_confirmation: String,
    recursive_delete: bool,
    privileged_copy_tree: bool,
    local_file: String,
    privileged_replace_existing: bool,
    privileged_approval: Option<String>,
    privileged_standing: bool,
    pub enabled: bool,
    pub schedule_enabled: bool,
    pub virtual_terminal: TerminalDraft,
    pub(super) journal_detail: JournalDetail,
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
        let recurring_rsync = super::rsync::standing::Settings::default();
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
            rsync_existing: None,
            rsync_allow_vacant_directory_download: false,
            rsync_allow_vacant_file_download: false,
            rsync_allow_vacant_server_file_upload: false,
            rsync_allow_vacant_server_tree_upload: false,
            rsync_mixed: false,
            rsync_elevation: super::rsync::Elevation::Ordinary,
            rsync_sources: text_editor::Content::new(),
            rsync_destinations: BTreeMap::new(),
            rsync_approval: None,
            rsync_standing: false,
            rsync_standing_id: None,
            rsync_standing_days: recurring_rsync.days.to_string(),
            rsync_standing_firings: recurring_rsync.firings.to_string(),
            rsync_standing_entries: recurring_rsync.budget.entries.to_string(),
            rsync_standing_mib: (recurring_rsync.budget.bytes as f64 / MIB).to_string(),
            rsync_standing_deletes: recurring_rsync.budget.deletes.to_string(),
            rsync_import: None,
            agent_goal: text_editor::Content::new(),
            agent_profile: None,
            imported_ai: None,
            imported_archive: None,
            imported_transfer: None,
            imported_privileged: None,
            imported_sudo_mode: None,
            agent_probe: super::agent::Probe::default(),
            swarm_parallelism: "4".into(),
            agent_approval: None,
            archive_format: ArchiveFormat::default(),
            archive_member_layout: ArchiveMemberLayout::default(),
            archive_server_only: false,
            archive_reuse_output: false,
            archive_password: None,
            archive_level: "6".into(),
            archive_download: String::new(),
            archive_uid: String::new(),
            archive_gid: String::new(),
            archive_mode: String::new(),
            reviewed_archive_uid: None,
            reviewed_archive_gid: None,
            remote_path: String::new(),
            remote_destination: String::new(),
            remote_mode: String::new(),
            remote_uid: String::new(),
            remote_gid: String::new(),
            reviewed_uid: None,
            reviewed_gid: None,
            remote_delete_confirmation: String::new(),
            recursive_delete: false,
            privileged_copy_tree: false,
            local_file: String::new(),
            privileged_replace_existing: false,
            privileged_approval: None,
            privileged_standing: false,
            enabled: true,
            schedule_enabled: false,
            virtual_terminal: TerminalDraft::from_options(None),
            journal_detail: JournalDetail::LimitedRedacted,
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
            agent_goal: text_editor::Content::with_text(&goal),
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

    pub(super) fn supports_journal_output(&self) -> bool {
        matches!(
            self.source,
            Source::Command | Source::Script | Source::Snippet
        ) || self
            .imported_privileged
            .as_deref()
            .is_some_and(|draft| draft.settings.shell_options().is_some())
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
            profile: self.automation_journal.summary_profile().cloned(),
            unresolved_profile: self.automation_journal.unresolved_summary_profile(),
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

    pub(super) fn ownership_target(&self) -> Option<&ConnectionRef> {
        if (self.source == Source::RemoteChown || self.server_output_enabled())
            && self.additional_connections.is_empty()
            && self.target_groups.is_none()
        {
            self.connection.as_ref()
        } else {
            None
        }
    }

    pub(super) fn apply_ownership_id(
        &mut self,
        kind: AccountKind,
        id: u32,
        route: Arc<ownership::Route>,
    ) {
        let (field, _) = self.owner_id_field(kind);
        self.set(field, id.to_string());
        let saved = match field {
            Field::ArchiveUid => &mut self.reviewed_archive_uid,
            Field::ArchiveGid => &mut self.reviewed_archive_gid,
            Field::RemoteUid => &mut self.reviewed_uid,
            Field::RemoteGid => &mut self.reviewed_gid,
            _ => unreachable!(),
        };
        *saved = Some(route);
    }

    fn owner_id_field(&self, kind: AccountKind) -> (Field, &str) {
        match (self.source, kind) {
            (Source::RemoteArchive, AccountKind::User) => (Field::ArchiveUid, &self.archive_uid),
            (Source::RemoteArchive, AccountKind::Group) => (Field::ArchiveGid, &self.archive_gid),
            (_, AccountKind::User) => (Field::RemoteUid, &self.remote_uid),
            (_, AccountKind::Group) => (Field::RemoteGid, &self.remote_gid),
        }
    }

    fn owner_id_entry(&self, kind: AccountKind, can_lookup: bool) -> Element<'_, Message> {
        let (field, value) = self.owner_id_field(kind);
        let placeholder = match kind {
            AccountKind::User => "Numeric UID (optional)",
            AccountKind::Group => "Numeric GID (optional)",
        };
        row![
            input(field, placeholder, value),
            button("Resolve Name…")
                .style(button::secondary)
                .on_press_maybe(
                    (can_lookup && self.ownership_target().is_some())
                        .then_some(Message::Ownership(ownership::Message::Start(kind)))
                ),
        ]
        .spacing(8)
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
            journal_detail: action.journal_detail,
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
            draft.agent_goal = text_editor::Content::with_text(&agent.goal);
            draft.agent_profile = Some(agent.profile.clone());
            draft.agent_probe = agent.probe;
            if agent.probe.supports_parallelism() {
                draft.swarm_parallelism = optional_text(agent.swarm_parallelism);
            }
            draft.agent_approval = agent.approval.clone();
        }
        if let Some(imported) = &action.imported_ai {
            draft.source = match imported.kind {
                super::agent::Kind::Agent => Source::AgentDiagnostics,
                super::agent::Kind::Swarm => Source::SwarmDiagnostics,
            };
            draft.agent_probe = imported.probe();
            draft.agent_goal = text_editor::Content::with_text(&imported.goal);
            draft.working_directory = imported.working_directory.clone().unwrap_or_default();
            draft.swarm_parallelism = imported.parallelism.to_string();
            draft.imported_ai = Some(Box::new(imported.clone()));
        }
        if let Some(imported) = &action.imported_transfer {
            draft.source = Source::ImportedTransfer;
            draft.imported_transfer = Some(imported.clone());
            if let imported_transfer::Draft::StagedSftp(staged) = imported.as_ref() {
                draft.remote_path.clone_from(&staged.settings.remote_hint);
            }
        }
        if let Some(imported) = &action.imported_privileged {
            draft.source = Source::ImportedPrivileged;
            draft.imported_privileged = Some(imported.clone());
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
                RemoteAction::ChmodSymbolic { path, mode } => {
                    draft.source = Source::RemoteChmod;
                    draft.remote_path = path.as_str().into();
                    draft.remote_mode = mode.to_string();
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
                        ProtectedOperation::ChmodSymbolic { mode } => {
                            draft.source = Source::RemoteChmod;
                            draft.remote_mode = mode.to_string();
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
                RemoteAction::ImportedArchive(imported) => {
                    draft.source = Source::RemoteArchive;
                    draft.archive_server_only = true;
                    draft.archive_format = imported.settings.format;
                    draft.archive_excludes = text_editor::Content::with_text(
                        &imported.settings.java_excludes.join("\n"),
                    );
                    draft.imported_archive = Some(imported.clone());
                }
                RemoteAction::Archive(archive)
                | RemoteAction::ServerArchive(archive)
                | RemoteAction::PrivilegedArchive(PrivilegedArchive { archive, .. }) => {
                    let Archive {
                        sources,
                        target,
                        format,
                        level,
                        password_credential,
                        excludes,
                        download,
                        server_output,
                        reuse_output,
                        member_layout,
                    } = archive;
                    draft.source = Source::RemoteArchive;
                    draft.archive_server_only = !matches!(remote, RemoteAction::Archive(_));
                    draft.archive_reuse_output = *reuse_output;
                    draft.archive_member_layout = *member_layout;
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
                    if let Some(output) = server_output {
                        draft.archive_uid = optional_text(output.uid);
                        draft.archive_gid = optional_text(output.gid);
                        draft.archive_mode = output
                            .mode
                            .map_or_else(String::new, |mode| format!("{mode:03o}"));
                    }
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
                    if let Some(settings) = &copy.standing {
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
                    draft.rsync_existing = transfer.existing.or(transfer.existing_download);
                    draft.rsync_allow_vacant_directory_download =
                        transfer.allow_vacant_directory_download;
                    draft.rsync_allow_vacant_file_download = transfer.allow_vacant_file_download;
                    draft.rsync_allow_vacant_server_file_upload =
                        transfer.allow_vacant_server_file_upload;
                    draft.rsync_allow_vacant_server_tree_upload =
                        transfer.allow_vacant_server_tree_upload;
                    draft.rsync_mixed = transfer.mixed.is_some();
                    draft.rsync_elevation = transfer.elevation;
                    draft.rsync_sources = text_editor::Content::with_text(
                        &transfer
                            .mixed
                            .as_ref()
                            .unwrap_or(&transfer.additional_sources)
                            .join("\n"),
                    );
                    draft.rsync_destinations = transfer
                        .routes
                        .iter()
                        .map(|route| (route.profile, route.destination.clone()))
                        .collect();
                    draft.rsync_approval = super::rsync::Reviewed::restore(transfer);
                    if let Some(config) = &transfer.standing {
                        draft.rsync_standing = true;
                        draft.rsync_standing_id = config.approval.clone();
                        draft.rsync_standing_days = config.settings.days.to_string();
                        draft.rsync_standing_firings = config.settings.firings.to_string();
                        draft.rsync_standing_entries = config.settings.budget.entries.to_string();
                        draft.rsync_standing_mib =
                            (config.settings.budget.bytes as f64 / MIB).to_string();
                        draft.rsync_standing_deletes = config.settings.budget.deletes.to_string();
                    }
                }
                RemoteAction::Privileged(transfer) => {
                    draft.source = match transfer.direction {
                        PrivilegedDirection::Upload => Source::RemoteUpload,
                        PrivilegedDirection::Download => Source::RemoteDownload,
                    };
                    draft.remote_path = transfer.remote.as_str().into();
                    draft.local_file = transfer.local.to_string_lossy().into_owned();
                    draft.privileged_replace_existing = transfer.replace_existing;
                    draft.privileged_approval = transfer.approval.clone();
                    if let Some(settings) = transfer.standing {
                        draft.privileged_standing = true;
                        draft.sync_standing_days = settings.days.to_string();
                        draft.sync_standing_firings = settings.limits.max_firings.to_string();
                        draft.sync_standing_mib =
                            (settings.limits.max_bytes as f64 / MIB).to_string();
                    }
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
            Field::ArchiveUid => &mut self.archive_uid,
            Field::ArchiveGid => &mut self.archive_gid,
            Field::ArchiveMode => &mut self.archive_mode,
            Field::RemoteDeleteConfirmation => &mut self.remote_delete_confirmation,
            Field::LocalFile => &mut self.local_file,
            Field::SyncStandingDays => &mut self.sync_standing_days,
            Field::SyncStandingFirings => &mut self.sync_standing_firings,
            Field::SyncStandingActions => &mut self.sync_standing_actions,
            Field::SyncStandingMib => &mut self.sync_standing_mib,
            Field::RsyncProgram => &mut self.rsync_program,
            Field::RsyncStandingDays => &mut self.rsync_standing_days,
            Field::RsyncStandingFirings => &mut self.rsync_standing_firings,
            Field::RsyncStandingEntries => &mut self.rsync_standing_entries,
            Field::RsyncStandingMib => &mut self.rsync_standing_mib,
            Field::RsyncStandingDeletes => &mut self.rsync_standing_deletes,
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
            match field {
                Field::RemoteUid => self.reviewed_uid = None,
                Field::RemoteGid => self.reviewed_gid = None,
                Field::ArchiveUid => self.reviewed_archive_uid = None,
                Field::ArchiveGid => self.reviewed_archive_gid = None,
                _ => {}
            }
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
                    && matches!(
                        field,
                        Field::RemotePath | Field::ArchiveLevel | Field::ArchiveDownload
                    ))
            {
                self.privileged_approval = None;
            }
            if self.privileged_standing
                && matches!(
                    field,
                    Field::SyncStandingDays | Field::SyncStandingFirings | Field::SyncStandingMib
                )
            {
                self.privileged_approval = None;
            }
            if matches!(field, Field::RsyncProgram)
                || matches!(
                    self.source,
                    Source::RsyncUpload | Source::RsyncDownloadTree | Source::RsyncUploadTree
                ) && matches!(field, Field::LocalFile | Field::RemotePath)
                || self.source == Source::RsyncDownload
                    && (self.rsync_existing.is_some()
                        || self.rsync_allow_vacant_file_download
                        || self.rsync_standing)
                    && matches!(field, Field::LocalFile | Field::RemotePath)
                || self.source.is_rsync()
                    && (!self.additional_connections.is_empty()
                        || !self.rsync_sources.text().trim().is_empty())
                    && matches!(field, Field::LocalFile | Field::RemotePath)
            {
                self.rsync_approval = None;
                self.rsync_standing_id = None;
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

    pub(super) fn suggest_agent_profile(&mut self, default: Option<&ProfileRef>) {
        if self.agent_profile.is_none() && !self.has_imported_ai() {
            self.agent_profile = default.cloned();
        }
    }

    pub(super) fn has_imported_ai(&self) -> bool {
        self.imported_ai.is_some()
    }

    pub(super) fn has_imported_transfer(&self) -> bool {
        self.imported_transfer.is_some()
    }

    pub(super) fn has_imported_archive(&self) -> bool {
        self.imported_archive.is_some()
    }

    pub(super) fn has_imported_privileged(&self) -> bool {
        self.imported_privileged.is_some()
    }

    fn imported_staging(&self) -> Option<&imported_transfer::StagedSftpDraft> {
        match self.imported_transfer.as_deref() {
            Some(imported_transfer::Draft::StagedSftp(staged)) => Some(staged),
            _ => None,
        }
    }

    pub(super) fn imported_route(&self) -> Option<&ConnectionRef> {
        self.imported_archive
            .as_ref()
            .map(|imported| &imported.route)
            .or_else(|| self.imported_staging().map(|imported| &imported.route))
    }

    pub(super) fn select_imported_sudo(&mut self, mode: SudoMode) {
        if (self.imported_staging().is_some() || self.has_imported_privileged())
            && mode != SudoMode::Disabled
        {
            self.imported_sudo_mode = Some(mode);
        }
    }

    pub(super) fn requires_import_review(&self) -> bool {
        self.has_imported_ai()
            || self.has_imported_transfer()
            || self.has_imported_archive()
            || self.has_imported_privileged()
    }

    pub(super) fn adopt_imported_privileged(
        &mut self,
        store: &ConnectionStore,
        shared: &super::SharedTargets,
        trust: &kortty_ssh::TrustStore,
        policy: &crate::clipboard_policy::ClipboardPolicy,
        snippets: &Path,
    ) -> Result<bool, &'static str> {
        use imported_privileged::Settings;

        let imported = self
            .imported_privileged
            .as_deref()
            .ok_or(imported_privileged::REVIEW_REQUIRED)?;
        let mode = self
            .imported_sudo_mode
            .ok_or("Choose passwordless or vault-backed sudo.")?;
        imported.validate(
            self.connection.iter().chain(&self.additional_connections),
            self.target_groups.is_some(),
        )?;
        self.require_current_groups(store, shared)?;
        imported.require_current_routes(store, shared, trust, policy)?;
        if let Settings::Snippet { link, .. } = &imported.settings {
            link.command(snippets)?;
        }

        let settings = self
            .imported_privileged
            .take()
            .expect("validated imported action")
            .settings;
        let file_action = settings.shell_options().is_none();
        if let Some(options) = settings.shell_options() {
            self.working_directory = options.working_directory.clone().unwrap_or_default();
            self.virtual_terminal = TerminalDraft::from_options(
                options
                    .virtual_terminal
                    .filter(|_| mode == SudoMode::Passwordless),
            );
        }
        match settings {
            Settings::Command { text, .. } => {
                self.source = Source::Command;
                self.command = text_editor::Content::with_text(&text);
            }
            Settings::Snippet { link, .. } => {
                self.source = Source::Snippet;
                self.snippet_id = Some(link.id);
                self.script_arguments = text_editor::Content::with_text(&link.arguments.join("\n"));
            }
            Settings::File { action } => self.set_imported_file_settings(action),
        }
        self.imported_sudo_mode = None;
        self.privileged_approval = None;
        self.sudo = mode;
        self.enabled = false;
        Ok(file_action)
    }

    fn set_imported_file_settings(&mut self, action: imported_privileged::FileAction) {
        use imported_privileged::FileAction;

        match action {
            FileAction::Mkdir { path } => {
                self.source = Source::RemoteMkdir;
                self.remote_path = path;
            }
            FileAction::Rename { from, to } => {
                self.source = Source::RemoteRename;
                self.remote_path = from;
                self.remote_destination = to;
            }
            FileAction::Chmod { path, permissions } => {
                self.source = Source::RemoteChmod;
                self.remote_path = path;
                self.remote_mode = permissions;
            }
            FileAction::Chown { path, owner, group } => {
                self.source = Source::RemoteChown;
                self.remote_path = path;
                self.remote_uid = owner;
                self.remote_gid = group.unwrap_or_default();
                self.reviewed_uid = None;
                self.reviewed_gid = None;
            }
            FileAction::Copy { from, to } => {
                self.source = Source::RemoteCopy;
                self.remote_path = from;
                self.remote_destination = to;
                self.privileged_copy_tree = false;
            }
            FileAction::Delete { path } => {
                self.source = Source::RemoteDelete;
                self.remote_path = path;
                self.recursive_delete = true;
                self.remote_delete_confirmation.clear();
            }
        }
    }

    pub(super) fn adopt_imported_archive(
        &mut self,
        store: &ConnectionStore,
        shared: &super::SharedTargets,
        trust: &kortty_ssh::TrustStore,
        policy: &crate::clipboard_policy::ClipboardPolicy,
        passwords: &[CredentialChoice],
    ) -> Result<(), &'static str> {
        let imported = self
            .imported_archive
            .as_ref()
            .ok_or(imported_archive::REVIEW_REQUIRED)?;
        if self.connection.as_ref() != Some(&imported.route)
            || !self.additional_connections.is_empty()
            || self.target_groups.is_some()
        {
            return Err("Keep the exact saved SSH route for the imported archive.");
        }
        imported.require_current_route(store, shared, trust, policy)?;
        let password = self.archive_password.as_ref().filter(|id| {
            imported.settings.format == ArchiveFormat::ZipPassword
                && passwords.iter().any(|choice| &choice.id == *id)
        });
        let archive = imported.adopt(
            self.archive_exclude_patterns(),
            imported.settings.local_hint.as_ref().and(
                (!self.archive_download.is_empty()).then(|| PathBuf::from(&self.archive_download)),
            ),
            password.cloned(),
        )?;
        self.archive_sources = text_editor::Content::with_text(
            &archive
                .sources
                .iter()
                .map(RemotePath::as_str)
                .collect::<Vec<_>>()
                .join("\n"),
        );
        self.remote_path = archive.target.as_str().to_owned();
        self.archive_format = archive.format;
        self.archive_member_layout = archive.member_layout;
        self.archive_level = archive.level.to_string();
        self.archive_download = archive
            .download
            .as_deref()
            .map_or_else(String::new, |path| path.to_string_lossy().into_owned());
        self.archive_password = archive.password_credential;
        self.archive_reuse_output = archive.reuse_output;
        self.sudo = if imported.settings.java_sudo {
            SudoMode::Passwordless
        } else {
            SudoMode::Disabled
        };
        self.privileged_approval = None;
        self.archive_uid.clear();
        self.archive_gid.clear();
        self.archive_mode.clear();
        self.reviewed_archive_uid = None;
        self.reviewed_archive_gid = None;
        self.imported_archive = None;
        self.enabled = false;
        Ok(())
    }

    pub(super) fn adopt_imported_transfer(
        &mut self,
        mode: imported_transfer::Mode,
    ) -> Result<(), &'static str> {
        use super::sync::Direction;
        use imported_transfer::Mode;

        if let Mode::Staged(destination) = mode {
            return self.adopt_imported_staging(destination);
        }
        let Some(imported_transfer::Draft::Sftp(imported)) = self.imported_transfer.as_deref()
        else {
            return Err(imported_transfer::REVIEW_REQUIRED);
        };
        if self.connection.is_none() || !self.additional_connections.is_empty() {
            return Err(
                "Choose one saved SSH target on the Job page before adopting the transfer.",
            );
        }
        imported.validate_adoption(mode, PathBuf::from(&self.local_file))?;
        self.source = match (mode, imported.kind.direction()) {
            (Mode::File, Direction::Upload) => Source::RemoteUpload,
            (Mode::File, Direction::Download) => Source::RemoteDownload,
            (Mode::Folder, Direction::Upload) => Source::RemoteUploadFolder,
            (Mode::Folder, Direction::Download) => Source::RemoteDownloadFolder,
            (Mode::Compare, Direction::Upload) => Source::SyncUpload,
            (Mode::Compare, Direction::Download) => Source::SyncDownload,
            (Mode::Staged(_), _) => unreachable!("staged transfers have a separate draft"),
        };
        self.remote_path = imported.remote.as_str().into();
        self.imported_transfer = None;
        self.enabled = false;
        self.sudo = SudoMode::Disabled;
        if mode == Mode::Compare {
            self.schedule_enabled = false;
        }
        Ok(())
    }

    fn adopt_imported_staging(
        &mut self,
        destination: imported_transfer::StagedDestination,
    ) -> Result<(), &'static str> {
        use imported_transfer::{Direction, StagedDestination};

        let staged = self
            .imported_staging()
            .ok_or(imported_transfer::STAGING_REVIEW_REQUIRED)?;
        staged.require_route(
            self.connection.as_ref(),
            &self.additional_connections,
            self.target_groups.is_some(),
        )?;
        let sudo = self
            .imported_sudo_mode
            .filter(|mode| *mode != SudoMode::Disabled)
            .ok_or("Choose passwordless or vault-backed sudo for this transfer.")?;
        let remote: RemotePath = self.remote_path.clone().try_into()?;
        staged.validate_adoption(destination, PathBuf::from(&self.local_file), remote)?;
        self.source = match (destination.is_folder(), staged.settings.kind.direction()) {
            (true, Direction::Upload) => Source::PrivilegedTreeUpload,
            (true, Direction::Download) => Source::PrivilegedTreeDownload,
            (false, Direction::Upload) => Source::RemoteUpload,
            (false, Direction::Download) => Source::RemoteDownload,
        };
        self.imported_transfer = None;
        self.imported_sudo_mode = None;
        self.enabled = false;
        self.sudo = sudo;
        self.privileged_replace_existing = destination == StagedDestination::ReplaceFile;
        self.privileged_standing = false;
        self.privileged_approval = None;
        self.sync_replace_changed = destination == StagedDestination::ExistingFolder;
        self.sync_approval = None;
        Ok(())
    }

    pub(super) fn configure_imported_rsync(
        &mut self,
        directory: bool,
        existing: bool,
        mixed: bool,
        elevation: super::rsync::Elevation,
    ) -> Result<(), &'static str> {
        use imported_transfer::{Direction, Draft};

        let Some(Draft::Rsync(imported)) = self.imported_transfer.as_deref() else {
            return Err(imported_transfer::RSYNC_REVIEW_REQUIRED);
        };
        let settings = if mixed {
            Some(imported.mixed_settings(elevation)?)
        } else if elevation == super::rsync::Elevation::Passwordless {
            imported.passwordless_settings(directory, existing)?
        } else if existing && !directory {
            imported.existing_local_file_source()?;
            Some(super::rsync::ExistingDestination::FILE)
        } else if existing {
            Some(imported.existing_settings()?)
        } else {
            imported.require_native_scope()?;
            None
        };
        if self.connection.is_none() {
            return Err("Choose a saved SSH target before configuring rsync.");
        }
        let server_file = if !directory && !mixed && imported.direction == Direction::Upload {
            if existing {
                Some(imported.server_file_path()?)
            } else if elevation == super::rsync::Elevation::Ordinary {
                imported.server_file_path().ok()
            } else {
                None
            }
        } else {
            None
        };
        self.source = match (imported.direction, directory || mixed) {
            (Direction::Download, false) => Source::RsyncDownload,
            (Direction::Download, true) => Source::RsyncDownloadTree,
            (Direction::Upload, false) => Source::RsyncUpload,
            (Direction::Upload, true) => Source::RsyncUploadTree,
        };
        self.rsync_program.clear();
        self.rsync_existing = settings;
        self.rsync_allow_vacant_directory_download = false;
        self.rsync_allow_vacant_file_download = false;
        self.rsync_allow_vacant_server_file_upload = false;
        self.rsync_allow_vacant_server_tree_upload = false;
        self.rsync_mixed = mixed;
        self.rsync_elevation = elevation;
        self.local_file.clear();
        self.remote_path.clear();
        self.rsync_sources = text_editor::Content::new();
        if mixed {
            self.rsync_sources = text_editor::Content::with_text(&imported.java_sources.join("\n"));
        } else if imported.direction == Direction::Download {
            self.remote_path = if existing || elevation == super::rsync::Elevation::Passwordless {
                imported.java_sources[0].trim_end_matches('/').to_owned()
            } else {
                imported.java_sources[0].clone()
            };
            self.rsync_sources =
                text_editor::Content::with_text(&imported.java_sources[1..].join("\n"));
        }
        if let Some(path) = server_file {
            self.remote_path = path.as_str().to_owned();
        } else if (existing || mixed) && imported.direction == Direction::Upload {
            let root = imported.java_target_root.trim_end_matches('/');
            if super::rsync::valid_remote(root) && RemotePath::try_from(root.to_owned()).is_ok() {
                self.remote_path = root.to_owned();
            }
        }
        if existing || mixed {
            self.schedule_enabled = false;
        }
        if elevation == super::rsync::Elevation::Passwordless {
            self.schedule_enabled = false;
            self.rsync_standing = false;
            self.rsync_standing_id = None;
        }
        self.rsync_destinations.clear();
        if imported.direction == Direction::Download {
            let grouped = !self.additional_connections.is_empty();
            let append_source_name = !mixed
                && imported.java_sources.len() == 1
                && (!directory || (!existing && !imported.java_sources[0].ends_with('/')));
            let default = |route: &ConnectionRef| {
                imported.download_destination(route.id, grouped, append_source_name)
            };
            self.local_file = self
                .connection
                .as_ref()
                .and_then(default)
                .unwrap_or_default();
            self.rsync_destinations.extend(
                self.additional_connections
                    .iter()
                    .filter_map(|route| Some((route.id, default(route)?))),
            );
        } else if !self.remote_path.is_empty() {
            self.rsync_destinations.extend(
                self.additional_connections
                    .iter()
                    .map(|route| (route.id, self.remote_path.clone())),
            );
        }
        self.rsync_approval = None;
        self.rsync_import = Some(Box::new(imported.clone()));
        self.imported_transfer = None;
        self.enabled = false;
        self.sudo = SudoMode::Disabled;
        Ok(())
    }

    pub(super) fn has_unresolved_imported_ai(&self) -> bool {
        self.has_imported_ai() && self.agent_profile.is_none()
    }

    pub(super) fn can_edit_agent_workflow(&self) -> bool {
        !self.has_unresolved_imported_ai()
    }

    fn rsync_transfer(&self) -> Result<super::rsync::Transfer, &'static str> {
        let upload = self.source.is_rsync_upload();
        let mixed = self
            .rsync_mixed
            .then(|| super::rsync::mixed::source_lines(&self.rsync_sources.text()));
        let first = mixed
            .as_ref()
            .and_then(|sources| sources.first())
            .map(|raw| super::rsync::mixed::bare_source(raw, upload));
        let local: &str = if upload {
            first.unwrap_or(&self.local_file)
        } else {
            &self.local_file
        };
        let remote: &str = if upload {
            &self.remote_path
        } else {
            first.unwrap_or(&self.remote_path)
        };
        Ok(super::rsync::Transfer {
            program: PathBuf::from(&self.rsync_program),
            local: PathBuf::from(local),
            remote: RemotePath::try_from(remote.to_owned())?,
            directory: matches!(
                self.source,
                Source::RsyncDownloadTree | Source::RsyncUploadTree
            ),
            existing: self.rsync_existing.filter(|_| {
                upload
                    && (self.source == Source::RsyncUploadTree
                        || self.supports_rsync_existing_file())
            }),
            existing_download: self.rsync_existing.filter(|_| {
                matches!(
                    self.source,
                    Source::RsyncDownload | Source::RsyncDownloadTree
                )
            }),
            allow_vacant_directory_download: self.rsync_allow_vacant_directory_download,
            allow_vacant_file_download: self.rsync_allow_vacant_file_download,
            allow_vacant_server_file_upload: self.rsync_allow_vacant_server_file_upload,
            allow_vacant_server_tree_upload: self.rsync_allow_vacant_server_tree_upload,
            additional_sources: if mixed.is_some() {
                Vec::new()
            } else {
                super::rsync::batch_sources(&self.rsync_sources.text())
            },
            routes: self
                .additional_connections
                .iter()
                .map(|reference| super::rsync::Route {
                    profile: reference.id,
                    destination: self
                        .rsync_destinations
                        .get(&reference.id)
                        .cloned()
                        .unwrap_or_default(),
                    approval: None,
                })
                .collect(),
            approval: None,
            standing: self
                .rsync_standing
                .then(|| self.rsync_standing_settings())
                .transpose()
                .map_err(|_| "Choose valid limits for recurring rsync.")?
                .map(|settings| super::rsync::standing::Config {
                    settings,
                    approval: self.rsync_standing_id.clone(),
                }),
            elevation: self.rsync_elevation,
            mixed,
        })
    }

    fn rsync_standing_settings(&self) -> Result<super::rsync::standing::Settings, String> {
        use super::rsync::standing::{Budget, Settings};

        let mib = self
            .rsync_standing_mib
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|mib| mib.is_finite() && (1.0 / MIB..=256.0).contains(mib))
            .ok_or("Enter the maximum MiB per rsync firing.")?;
        let settings = Settings {
            days: self
                .rsync_standing_days
                .trim()
                .parse()
                .map_err(|_| "Enter the number of days for the recurring rsync approval.")?,
            firings: self
                .rsync_standing_firings
                .trim()
                .parse()
                .map_err(|_| "Enter the number of firings for the recurring rsync approval.")?,
            budget: Budget {
                bytes: (mib * MIB).floor() as u64,
                entries: self
                    .rsync_standing_entries
                    .trim()
                    .parse()
                    .map_err(|_| "Enter the maximum entries per rsync firing.")?,
                deletes: self
                    .rsync_standing_deletes
                    .trim()
                    .parse()
                    .map_err(|_| "Enter the maximum deletions per rsync firing.")?,
            },
        };
        settings.validate().map_err(|error| error.to_string())?;
        Ok(settings)
    }

    pub(super) fn rsync_standing_intent(
        &self,
    ) -> Result<Option<super::rsync::standing::Intent>, String> {
        use super::rsync::standing::{Intent, Triggers};

        if !self.rsync_standing {
            return Ok(None);
        }
        let triggers = Triggers {
            schedule: Some(self.schedule()?),
            mode: self.mode.into(),
            predecessors: self.dependencies.iter().cloned().collect(),
            max_age_minutes: optional_parse(
                &self.age,
                "Enter a positive dependency age in minutes",
            )?,
        };
        triggers.validate()?;
        let zone = kortty_jobs::TimeZone::try_system().unwrap_or(kortty_jobs::TimeZone::UTC);
        let zone = zone
            .iana_name()
            .ok_or("Choose a named time zone for recurring rsync.")?
            .into();
        Ok(Some(Intent {
            settings: self.rsync_standing_settings()?,
            triggers,
            zone,
        }))
    }

    pub(super) fn set_rsync_standing(&mut self, enabled: bool) {
        if self.rsync_standing != enabled {
            self.rsync_approval = None;
            self.rsync_standing_id = None;
            self.rsync_standing = enabled;
            if !enabled {
                if self.rsync_allow_vacant_server_file_upload
                    && self.rsync_elevation == super::rsync::Elevation::Ordinary
                {
                    self.rsync_existing = None;
                }
                self.rsync_allow_vacant_server_file_upload = false;
                self.rsync_allow_vacant_server_tree_upload = false;
            }
        }
    }

    pub(super) fn set_rsync_standing_id(&mut self, id: String) {
        self.rsync_standing_id = Some(id);
    }

    pub(super) fn set_rsync_elevation(&mut self, enabled: bool) -> Result<bool, &'static str> {
        use super::rsync::Elevation;

        if !self.source.is_rsync() {
            return Ok(false);
        }
        if enabled && !self.rsync_mixed && !self.rsync_sources.text().trim().is_empty() {
            return Err("Choose mixed source transfer to review multiple passwordless sources.");
        }
        if self
            .rsync_import
            .as_ref()
            .is_some_and(|source| source.java_sudo != enabled)
        {
            return Err("Keep the passwordless elevation mode of the imported Java action.");
        }
        if !enabled
            && self.source == Source::RsyncUpload
            && self.rsync_existing.is_some()
            && !self.rsync_allow_vacant_server_file_upload
        {
            return Err("Clear the existing file destination before choosing ordinary rsync.");
        }
        let elevation = if enabled {
            Elevation::Passwordless
        } else {
            Elevation::Ordinary
        };
        if self.rsync_elevation == elevation {
            return Ok(false);
        }
        self.rsync_elevation = elevation;
        self.rsync_approval = None;
        self.rsync_standing_id = None;
        Ok(true)
    }

    pub(super) fn rsync_candidate(&self) -> Option<super::rsync::ReviewRequests> {
        if !self.source.is_rsync() {
            return None;
        }
        let transfer = self.rsync_transfer().ok()?;
        if !self.rsync_import_matches(&transfer) {
            return None;
        }
        super::rsync::ReviewRequests::for_transfer(
            &transfer,
            self.connection.as_ref()?,
            &self.additional_connections,
            self.source.is_rsync_upload(),
        )
        .ok()
    }

    fn rsync_import_matches(&self, transfer: &super::rsync::Transfer) -> bool {
        self.rsync_import.as_ref().is_none_or(|imported| {
            if transfer.uses_passwordless_sudo() != imported.java_sudo {
                return false;
            }
            if let Some(sources) = &transfer.mixed {
                if imported.mixed_settings(transfer.elevation).ok()
                    != transfer.existing.or(transfer.existing_download)
                {
                    return false;
                }
                let contents = |raw: &String| {
                    super::rsync::mixed::source_contents(raw, self.source.is_rsync_upload())
                };
                return sources
                    .iter()
                    .map(contents)
                    .eq(imported.java_sources.iter().map(contents));
            }
            if imported.java_sudo
                && imported
                    .passwordless_settings(transfer.directory, transfer.uses_existing_destination())
                    .ok()
                    != Some(transfer.existing.or(transfer.existing_download))
            {
                return false;
            }
            if !transfer.directory && transfer.existing_download.is_some() {
                let expected = Some(std::ffi::OsStr::new(transfer.remote.basename()));
                if imported.existing_local_file_source().is_err()
                    || transfer.local.file_name() != expected
                    || transfer
                        .routes
                        .iter()
                        .any(|route| Path::new(&route.destination).file_name() != expected)
                {
                    return false;
                }
            }
            if imported.java_sudo
                && transfer.directory
                && !transfer.uses_existing_destination()
                && !imported.java_sources[0].ends_with('/')
            {
                let upload = self.source.is_rsync_upload();
                let (name, destination) = if upload {
                    (
                        transfer.local.file_name(),
                        Path::new(transfer.remote.as_str()),
                    )
                } else {
                    (
                        Some(std::ffi::OsStr::new(transfer.remote.basename())),
                        transfer.local.as_path(),
                    )
                };
                if name.is_none()
                    || destination.file_name() != name
                    || transfer
                        .routes
                        .iter()
                        .any(|route| Path::new(&route.destination).file_name() != name)
                {
                    return false;
                }
            }
            imported.java_sources.len() == 1 + transfer.additional_sources.len()
        })
    }

    pub(super) fn rsync_groups(&self) -> Option<TargetGroups> {
        self.target_groups.clone()
    }

    pub(super) fn approve_rsync(&mut self, approval: super::rsync::Reviewed) -> bool {
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

    pub(super) fn set_rsync_destination(&mut self, profile: u64, destination: String) -> bool {
        if !self.source.is_rsync()
            || destination.len() > MAX_FIELD_BYTES
            || !self
                .additional_connections
                .iter()
                .any(|reference| reference.id == profile)
            || self.rsync_destinations.get(&profile) == Some(&destination)
        {
            return false;
        }
        self.rsync_destinations.insert(profile, destination);
        self.rsync_approval = None;
        true
    }

    pub(super) fn change_rsync_existing(
        &mut self,
        change: impl FnOnce(
            Option<super::rsync::ExistingDestination>,
        ) -> Option<super::rsync::ExistingDestination>,
    ) -> bool {
        let file = self.supports_rsync_existing_file();
        if !(self.supports_rsync_existing() || file) || self.rsync_import.is_some() {
            return false;
        }
        let settings = change(self.rsync_existing);
        if settings == self.rsync_existing
            || file
                && settings
                    .is_some_and(|settings| settings != super::rsync::ExistingDestination::FILE)
            || self.rsync_mixed && settings.is_none_or(|settings| settings.include_source_name)
            || !self.rsync_mixed
                && settings.is_some()
                && !self.rsync_sources.text().trim().is_empty()
        {
            return false;
        }
        self.rsync_existing = settings;
        if settings.is_none() {
            self.rsync_allow_vacant_directory_download = false;
            self.rsync_allow_vacant_server_file_upload = false;
            self.rsync_allow_vacant_server_tree_upload = false;
        } else {
            self.rsync_allow_vacant_file_download = false;
        }
        self.rsync_approval = None;
        self.rsync_standing_id = None;
        true
    }

    pub(super) fn choose_existing_rsync_destination(&mut self, enabled: bool) -> bool {
        let initial = if self.supports_rsync_existing_file() {
            super::rsync::ExistingDestination::FILE
        } else {
            super::rsync::ExistingDestination {
                delete: false,
                include_source_name: true,
            }
        };
        self.change_rsync_existing(|settings| enabled.then(|| settings.unwrap_or(initial)))
    }

    pub(super) fn set_rsync_allow_vacant_directory_download(&mut self, enabled: bool) -> bool {
        if !self.supports_vacant_rsync_directory()
            || self.rsync_allow_vacant_directory_download == enabled
        {
            return false;
        }
        self.rsync_allow_vacant_directory_download = enabled;
        self.rsync_approval = None;
        self.rsync_standing_id = None;
        true
    }

    pub(super) fn set_rsync_allow_vacant_file_download(&mut self, enabled: bool) -> bool {
        if (enabled && !self.supports_vacant_rsync_file())
            || self.rsync_allow_vacant_file_download == enabled
        {
            return false;
        }
        self.rsync_allow_vacant_file_download = enabled;
        self.rsync_approval = None;
        self.rsync_standing_id = None;
        true
    }

    pub(super) fn set_rsync_allow_vacant_server_file_upload(&mut self, enabled: bool) -> bool {
        if (enabled && !self.supports_vacant_rsync_server_file())
            || self.rsync_allow_vacant_server_file_upload == enabled
        {
            return false;
        }
        self.rsync_allow_vacant_server_file_upload = enabled;
        if self.rsync_elevation == super::rsync::Elevation::Ordinary {
            self.rsync_existing = enabled.then_some(super::rsync::ExistingDestination::FILE);
        }
        self.rsync_approval = None;
        self.rsync_standing_id = None;
        true
    }

    pub(super) fn set_rsync_allow_vacant_server_tree_upload(&mut self, enabled: bool) -> bool {
        if (enabled && !self.supports_vacant_rsync_server_tree())
            || self.rsync_allow_vacant_server_tree_upload == enabled
        {
            return false;
        }
        self.rsync_allow_vacant_server_tree_upload = enabled;
        self.rsync_approval = None;
        self.rsync_standing_id = None;
        true
    }

    pub(super) fn change_rsync_mixed(&mut self, enabled: bool) -> bool {
        if !super::rsync::LOCAL_EXISTING_SUPPORTED
            || !self.supports_rsync_existing()
            || self.rsync_import.is_some()
            || enabled == self.rsync_mixed
        {
            return false;
        }
        if enabled {
            let first = if self.source.is_rsync_upload() {
                &self.local_file
            } else {
                &self.remote_path
            };
            let suffix = if !first.is_empty()
                && !super::rsync::mixed::source_contents(first, self.source.is_rsync_upload())
                && self
                    .rsync_existing
                    .is_some_and(|settings| !settings.include_source_name)
            {
                "/"
            } else {
                ""
            };
            let sources = format!("{first}{suffix}\n{}", self.rsync_sources.text());
            self.rsync_sources = text_editor::Content::with_text(&sources);
            self.rsync_existing = Some(super::rsync::ExistingDestination {
                delete: self.rsync_existing.is_some_and(|settings| settings.delete),
                include_source_name: false,
            });
        } else {
            self.rsync_existing = None;
            self.rsync_sources = text_editor::Content::new();
            if self.source.is_rsync_upload() {
                self.local_file.clear();
            } else {
                self.remote_path.clear();
            }
        }
        self.rsync_mixed = enabled;
        self.rsync_allow_vacant_directory_download = false;
        self.rsync_allow_vacant_file_download = false;
        self.rsync_allow_vacant_server_file_upload = false;
        self.rsync_allow_vacant_server_tree_upload = false;
        self.rsync_standing_id = None;
        self.rsync_approval = None;
        true
    }

    fn supports_rsync_existing(&self) -> bool {
        self.source == Source::RsyncUploadTree
            || self.source == Source::RsyncDownloadTree && super::rsync::LOCAL_EXISTING_SUPPORTED
    }

    fn supports_vacant_rsync_directory(&self) -> bool {
        self.source == Source::RsyncDownloadTree
            && super::rsync::LOCAL_EXISTING_SUPPORTED
            && self.rsync_existing.is_some()
            && (self.rsync_mixed || self.rsync_sources.text().trim().is_empty())
    }

    fn supports_rsync_existing_file(&self) -> bool {
        self.source == Source::RsyncUpload
            && (self.rsync_elevation == super::rsync::Elevation::Passwordless
                || self.rsync_allow_vacant_server_file_upload)
            || self.source == Source::RsyncDownload && super::rsync::LOCAL_EXISTING_SUPPORTED
    }

    fn supports_vacant_rsync_file(&self) -> bool {
        self.source == Source::RsyncDownload
            && super::rsync::LOCAL_EXISTING_SUPPORTED
            && self.rsync_existing.is_none()
            && !self.rsync_mixed
            && self.rsync_sources.text().trim().is_empty()
    }

    fn supports_vacant_rsync_server_file(&self) -> bool {
        self.source == Source::RsyncUpload
            && self.rsync_standing
            && (self.supports_ordinary_rsync_server_file()
                || self.rsync_existing == Some(super::rsync::ExistingDestination::FILE))
            && !self.rsync_mixed
            && self.rsync_sources.text().trim().is_empty()
    }

    fn supports_ordinary_rsync_server_file(&self) -> bool {
        self.source == Source::RsyncUpload
            && self.rsync_elevation == super::rsync::Elevation::Ordinary
            && !self.rsync_mixed
            && self.rsync_sources.text().trim().is_empty()
    }

    fn supports_vacant_rsync_server_tree(&self) -> bool {
        self.source == Source::RsyncUploadTree
            && self.rsync_standing
            && self.rsync_existing.is_some()
            && (self.rsync_mixed || self.rsync_sources.text().trim().is_empty())
    }

    pub(super) fn edit_sync_excludes(&mut self, action: text_editor::Action) {
        edit_bounded(&mut self.sync_excludes, action, MAX_ARCHIVE_EXCLUDE_TEXT);
    }

    pub(super) fn edit_rsync_sources(&mut self, action: text_editor::Action) -> bool {
        if (self.rsync_existing.is_some() && !self.rsync_mixed)
            || self.rsync_allow_vacant_file_download
        {
            return false;
        }
        let changed = edit_bounded(
            &mut self.rsync_sources,
            action,
            super::rsync::MAX_SOURCE_TEXT,
        );
        if changed {
            self.rsync_approval = None;
        }
        changed
    }

    pub(super) fn set_sync_standing(&mut self, enabled: bool) {
        if self.sync_standing != enabled {
            self.sync_approval = None;
        }
        self.sync_standing = enabled;
    }

    pub(super) fn set_sync_replace_changed(&mut self, enabled: bool) {
        if !matches!(
            self.source,
            Source::SyncUpload
                | Source::SyncDownload
                | Source::PrivilegedTreeUpload
                | Source::PrivilegedTreeDownload
        ) {
            return;
        }
        if self.sync_replace_changed != enabled {
            self.sync_approval = None;
        }
        self.sync_replace_changed = enabled;
    }

    fn sync_standing_settings(&self, replacement: bool) -> Result<Settings, String> {
        let maximum = if replacement {
            MAX_REPLACEMENT_BYTES
        } else {
            MAX_BYTES
        };
        let bytes = self
            .sync_standing_mib
            .trim()
            .parse::<f64>()
            .ok()
            .map(|mib| mib * MIB)
            .filter(|bytes| (1.0..=maximum as f64).contains(bytes) && bytes.fract() == 0.0)
            .ok_or_else(|| format!(
                "Enter an SFTP transfer limit up to {} MiB that represents a whole number of bytes",
                maximum as f64 / MIB,
            ))?;
        let settings = Settings {
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
                max_actions: if replacement {
                    1
                } else {
                    self.sync_standing_actions.trim().parse().map_err(
                        |_| "Enter the maximum number of files and folders per SFTP firing",
                    )?
                },
                max_bytes: bytes as u64,
            },
        };
        if replacement {
            settings.validate_replacement()
        } else {
            settings.validate()
        }
        .map_err(|error| error.to_string())?;
        Ok(settings)
    }

    fn protected_tree_standing_view(&self) -> Element<'_, Message> {
        column![
            checkbox(self.sync_standing).label("Allow recurring protected folder transfers on this device")
                .on_toggle(Message::SyncStanding),
            self.sync_standing.then(|| self.standing_limits_view(false)),
            text(if self.sync_standing {
                "Each approved calendar or Chain firing may create or replace eligible entries in the reviewed receiver. Originals remain in private recovery folders. An unconfirmed run requires a new review. Downloads charge the complete source payload, including unchanged files."
            } else {
                "Save one approval for Run Now or one scheduled date and time. With recurring approval, an absent receiver is created once; later firings merge into that exact receiver."
            }).size(12),
        ].spacing(6).into()
    }

    fn standing_limits_view(&self, replacement: bool) -> Element<'_, Message> {
        column![
            text("Enable a calendar or choose Chain with predecessors on the Job page.").size(12),
            form_row(
                "Approval duration (days)",
                input(Field::SyncStandingDays, "Days", &self.sync_standing_days)
            ),
            form_row(
                "Maximum firings",
                input(
                    Field::SyncStandingFirings,
                    "Firings",
                    &self.sync_standing_firings
                )
            ),
            (!replacement).then(|| form_row(
                "Files and folders per firing",
                input(
                    Field::SyncStandingActions,
                    "Maximum count",
                    &self.sync_standing_actions
                )
            )),
            form_row(
                "MiB per firing",
                input(
                    Field::SyncStandingMib,
                    "Maximum MiB",
                    &self.sync_standing_mib
                )
            ),
        ]
        .spacing(6)
        .into()
    }

    pub fn select_source(&mut self, source: Source) {
        if self.has_imported_archive()
            || self.has_imported_transfer()
            || self.has_imported_privileged()
            || matches!(
                source,
                Source::ImportedTransfer | Source::ImportedPrivileged
            )
            || self.rsync_import.is_some()
                && (!source.is_rsync()
                    || self.source.is_rsync_upload() != source.is_rsync_upload()
                    || self.rsync_existing.is_some() && source != self.source)
        {
            return;
        }
        if self.source != source {
            if self.imported_ai.take().is_some() {
                self.agent_profile = None;
                self.agent_approval = None;
                self.agent_goal = text_editor::Content::new();
            }
            self.remote_delete_confirmation.clear();
            self.recursive_delete = false;
            self.privileged_copy_tree = false;
            self.privileged_replace_existing = false;
            self.privileged_standing = false;
            self.privileged_approval = None;
            if self.source.is_rsync() || source.is_rsync() {
                self.rsync_approval = None;
                self.rsync_existing = None;
                self.rsync_allow_vacant_directory_download = false;
                self.rsync_allow_vacant_file_download = false;
                self.rsync_allow_vacant_server_file_upload = false;
                self.rsync_allow_vacant_server_tree_upload = false;
                if self.rsync_mixed {
                    self.rsync_mixed = false;
                    self.rsync_sources = text_editor::Content::new();
                }
                if !source.is_rsync() || self.source.is_rsync_upload() != source.is_rsync_upload() {
                    self.rsync_destinations.clear();
                    self.rsync_import = None;
                    self.rsync_elevation = super::rsync::Elevation::Ordinary;
                }
            }
            if matches!(
                self.source,
                Source::SyncUpload
                    | Source::SyncDownload
                    | Source::PrivilegedTreeUpload
                    | Source::PrivilegedTreeDownload
            ) || matches!(
                source,
                Source::SyncUpload
                    | Source::SyncDownload
                    | Source::PrivilegedTreeUpload
                    | Source::PrivilegedTreeDownload
            ) {
                self.sync_replace_changed = false;
                if self.privileged_tree()
                    || matches!(
                        source,
                        Source::PrivilegedTreeUpload | Source::PrivilegedTreeDownload
                    )
                {
                    self.sync_standing = false;
                }
                self.sync_approval = None;
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

    fn server_output_enabled(&self) -> bool {
        self.privileged_archive() && self.sudo == SudoMode::Disabled
    }

    fn require_reviewed_ownership(
        &self,
        routes: [&Option<Arc<ownership::Route>>; 2],
        authorize: &impl Fn(&ownership::Route, SudoMode) -> Result<(), &'static str>,
    ) -> Result<(), &'static str> {
        for route in routes.into_iter().flatten() {
            if !self
                .ownership_target()
                .is_some_and(|target| route.matches(target))
            {
                return Err(
                    "Review the account again for the selected route, or edit its numeric ID.",
                );
            }
            authorize(route, self.sudo)?;
        }
        Ok(())
    }

    fn server_output(
        &self,
        authorize: &impl Fn(&ownership::Route, SudoMode) -> Result<(), &'static str>,
    ) -> Result<Option<kortty_server_archive::protocol::OutputMetadata>, &'static str> {
        use kortty_server_archive::protocol::OutputMetadata;

        if !self.server_output_enabled() {
            return Ok(None);
        }
        self.require_reviewed_ownership(
            [&self.reviewed_archive_uid, &self.reviewed_archive_gid],
            authorize,
        )?;
        let mode = self.archive_mode.trim();
        let mode = match mode.len() {
            0 => None,
            3 => Some(mode_from_text(mode)?),
            _ => return Err("Enter exactly three octal digits for archive permissions."),
        };
        let uid = owner_id_from_text(&self.archive_uid)?;
        let gid = owner_id_from_text(&self.archive_gid)?;
        Ok(
            (uid.is_some() || gid.is_some() || mode.is_some()).then_some(OutputMetadata {
                uid,
                gid,
                mode,
            }),
        )
    }

    pub(super) fn select_sudo(&mut self, mode: SudoMode) {
        if self.sudo == mode {
            return;
        }
        self.privileged_approval = None;
        if mode == SudoMode::Disabled {
            self.privileged_replace_existing = false;
            self.privileged_standing = false;
        }
        self.sudo = mode;
    }

    fn can_replace_privileged_file(&self) -> bool {
        self.regular_transfer() && self.sudo != SudoMode::Disabled
    }

    pub(super) fn set_privileged_replace_existing(&mut self, enabled: bool) -> bool {
        if !self.can_replace_privileged_file() || self.privileged_replace_existing == enabled {
            return false;
        }
        self.privileged_replace_existing = enabled;
        self.privileged_standing = false;
        self.privileged_approval = None;
        true
    }

    pub(super) fn set_privileged_standing(&mut self, enabled: bool) -> bool {
        if !self.can_replace_privileged_file()
            || !self.privileged_replace_existing
            || self.privileged_standing == enabled
        {
            return false;
        }
        self.privileged_standing = enabled;
        self.privileged_approval = None;
        true
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
            self.rsync_approval = None;
            if self.privileged_archive() {
                self.privileged_approval = None;
            }
            self.connection = connection.cloned();
            if let Some(reference) = connection {
                self.additional_connections
                    .retain(|other| !other.same_identity(reference));
                self.rsync_destinations.remove(&reference.id);
            } else {
                self.additional_connections.clear();
                self.rsync_destinations.clear();
            }
        }
    }

    pub(super) fn load_reviewed_shared_targets(
        &self,
        store: &ConnectionStore,
    ) -> Result<super::SharedTargets, &'static str> {
        if self.has_teamwork_targets() {
            return super::SharedTargets::load_reviewed(
                store,
                self.connection
                    .iter()
                    .chain(&self.additional_connections)
                    .cloned(),
            );
        }
        Ok(super::SharedTargets::default())
    }

    pub(super) fn require_current_shared_targets(
        &self,
        shared: &super::SharedTargets,
    ) -> Result<(), &'static str> {
        for reference in self.connection.iter().chain(&self.additional_connections) {
            shared.require(reference)?;
        }
        Ok(())
    }

    pub(super) fn add_group_connection(
        &mut self,
        choice: ConnectionChoice,
        store: &ConnectionStore,
        shared: &super::SharedTargets,
    ) -> Result<bool, &'static str> {
        let mut source = self
            .target_groups
            .clone()
            .ok_or("Select a connection group first.")?;
        let reference = choice.reference().ok_or("Choose a saved SSH connection.")?;
        if let Some(selected) = source
            .explicit
            .iter_mut()
            .find(|target| target.same_identity(reference))
        {
            selected.clone_from(reference);
        } else {
            source.explicit.push(reference.clone());
        }
        self.apply_target_groups(source, store, shared)
    }

    pub(super) fn remove_group_connection(
        &mut self,
        id: u64,
        store: &ConnectionStore,
        shared: &super::SharedTargets,
    ) -> (bool, Option<&'static str>) {
        let Some(mut source) = self.target_groups.clone() else {
            return (false, None);
        };
        let before = source.explicit.len();
        source.explicit.retain(|target| target.id != id);
        if source.explicit.len() == before {
            return (false, None);
        }
        self.remove_group_selection(source, store, shared)
    }

    pub(super) fn has_teamwork_targets(&self) -> bool {
        self.connection
            .iter()
            .chain(&self.additional_connections)
            .any(|reference| reference.shared_identity().is_some())
            || self
                .target_groups
                .as_ref()
                .is_some_and(TargetGroups::has_teamwork_groups)
    }

    pub(super) fn has_connection_groups(&self) -> bool {
        self.target_groups.is_some()
    }

    pub(super) fn require_current_groups(
        &self,
        store: &ConnectionStore,
        shared: &super::SharedTargets,
    ) -> Result<(), &'static str> {
        if let Some(source) = &self.target_groups {
            let resolved =
                source.resolve(store, shared, self.source == Source::SwarmDiagnostics)?;
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
        group: super::ConnectionGroup,
        store: &ConnectionStore,
        shared: &super::SharedTargets,
    ) -> Result<bool, &'static str> {
        let mut source = self.target_groups.clone().unwrap_or_else(|| TargetGroups {
            group_order: Vec::new(),
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
        self.apply_target_groups(source, store, shared)
    }

    pub(super) fn remove_connection_group(
        &mut self,
        group: &super::ConnectionGroup,
        store: &ConnectionStore,
        shared: &super::SharedTargets,
    ) -> (bool, Option<&'static str>) {
        let Some(mut source) = self.target_groups.clone() else {
            return (false, None);
        };
        let before = source.groups.len();
        source.groups.retain(|path| path != group);
        if source.groups.len() == before {
            return (false, None);
        }
        self.remove_group_selection(source, store, shared)
    }

    pub(super) fn refresh_connection_groups(
        &mut self,
        store: &ConnectionStore,
        shared: &super::SharedTargets,
    ) -> Result<bool, &'static str> {
        let Some(source) = self.target_groups.clone() else {
            return Ok(false);
        };
        self.apply_target_groups(source, store, shared)
    }

    fn apply_target_groups(
        &mut self,
        source: TargetGroups,
        store: &ConnectionStore,
        shared: &super::SharedTargets,
    ) -> Result<bool, &'static str> {
        let targets = self.resolve_group_selection(&source, store, shared)?;
        Ok(self.set_group_selection(source, targets, false))
    }

    fn remove_group_selection(
        &mut self,
        source: TargetGroups,
        store: &ConnectionStore,
        shared: &super::SharedTargets,
    ) -> (bool, Option<&'static str>) {
        match self.resolve_group_selection(&source, store, shared) {
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
        shared: &super::SharedTargets,
    ) -> Result<Vec<ConnectionRef>, &'static str> {
        if source.groups.is_empty() {
            Ok(source.explicit.clone())
        } else {
            source.resolve(store, shared, self.source == Source::SwarmDiagnostics)
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
        self.rsync_destinations.retain(|id, _| {
            self.additional_connections
                .iter()
                .any(|target| target.id == *id)
        });
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
        if primary.same_identity(reference) {
            return Err("This SSH connection is already the primary target");
        }
        if let Some(current) = self
            .additional_connections
            .iter_mut()
            .find(|other| other.same_identity(reference))
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
        self.rsync_approval = None;
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
        self.rsync_destinations.remove(&id);
        self.rsync_approval = None;
        self.remote_delete_confirmation.clear();
        true
    }

    pub fn edit_command(&mut self, action: text_editor::Action) {
        edit_bounded(&mut self.command, action, MAX_COMMAND_BYTES);
    }

    fn agent_goal_limit(&self) -> usize {
        if self.has_imported_ai() {
            MAX_COMMAND_BYTES
        } else {
            super::agent::MAX_GOAL_BYTES
        }
    }

    pub(super) fn edit_agent_goal(&mut self, action: text_editor::Action) -> bool {
        let limit = self.agent_goal_limit();
        edit_bounded(&mut self.agent_goal, action, limit)
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
        if self.has_imported_archive()
            || self.archive_format == format
            || !self.archive_formats().contains(&format)
        {
            return false;
        }
        self.archive_format = format;
        if !format.supports_server_member_paths() {
            self.archive_member_layout = ArchiveMemberLayout::SourceBasename;
        }
        self.privileged_approval = None;
        true
    }

    pub(super) fn select_archive_password(&mut self, id: String) -> bool {
        if self.archive_password.as_deref() == Some(id.as_str()) {
            return false;
        }
        self.archive_password = Some(id);
        self.privileged_approval = None;
        true
    }

    pub(super) fn set_archive_server_only(&mut self, enabled: bool) -> bool {
        if self.has_imported_archive()
            || self.source != Source::RemoteArchive
            || self.archive_server_only == enabled
        {
            return false;
        }
        self.archive_server_only = enabled;
        self.archive_reuse_output = false;
        self.archive_member_layout = ArchiveMemberLayout::SourceBasename;
        self.sudo = SudoMode::Disabled;
        self.privileged_approval = None;
        if !self.archive_formats().contains(&self.archive_format) {
            self.archive_format = ArchiveFormat::Zip;
        }
        true
    }

    pub(super) fn set_archive_server_path(&mut self, preserve: bool) -> bool {
        let layout = if preserve {
            ArchiveMemberLayout::ServerPath
        } else {
            ArchiveMemberLayout::SourceBasename
        };
        if !self.privileged_archive()
            || self.has_imported_archive()
            || !self.archive_format.supports_server_member_paths()
            || self.archive_member_layout == layout
        {
            return false;
        }
        self.archive_member_layout = layout;
        self.privileged_approval = None;
        true
    }

    pub(super) fn set_archive_reuse_output(&mut self, enabled: bool) -> bool {
        if !self.privileged_archive()
            || self.has_imported_archive()
            || self.archive_reuse_output == enabled
        {
            return false;
        }
        self.archive_reuse_output = enabled;
        self.privileged_approval = None;
        true
    }

    fn archive_formats(&self) -> &'static [ArchiveFormat] {
        if self.archive_server_only {
            &ArchiveFormat::SERVER_ONLY
        } else {
            &ArchiveFormat::ALL
        }
    }

    fn archive_exclude_patterns(&self) -> Vec<String> {
        self.archive_excludes
            .text()
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect()
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
        self.build_with_ownership_check(id, ownership::Route::authorize)
    }

    pub(super) fn build_with_ownership_check(
        &self,
        id: JobId,
        authorize: impl Fn(&ownership::Route, SudoMode) -> Result<(), &'static str>,
    ) -> Result<Job<SshCommand>, String> {
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
            imported_ai: None,
            imported_transfer: self.imported_transfer.clone(),
            imported_privileged: self.imported_privileged.clone(),
            sudo: self.sudo,
            virtual_terminal: if self.source.is_remote() || self.source.is_agent() {
                None
            } else {
                self.virtual_terminal.build()?
            },
            journal_detail: if self.supports_journal_output() {
                self.journal_detail
            } else {
                JournalDetail::Off
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
            Source::ImportedTransfer | Source::ImportedPrivileged => {}
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
                let path = remote_path()?;
                action.remote = Some(match mode_from_text(&self.remote_mode) {
                    Ok(mode) => RemoteAction::Chmod { path, mode },
                    Err(_) => RemoteAction::ChmodSymbolic {
                        path,
                        mode: SymbolicMode::try_from(self.remote_mode.trim().to_owned())?,
                    },
                })
            }
            Source::RemoteChown => {
                self.require_reviewed_ownership(
                    [&self.reviewed_uid, &self.reviewed_gid],
                    &authorize,
                )?;
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
            Source::RemoteArchive if self.has_imported_archive() => {
                action.remote = self
                    .imported_archive
                    .clone()
                    .map(RemoteAction::ImportedArchive);
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
                    password_credential: self
                        .archive_password
                        .clone()
                        .filter(|_| self.archive_format == ArchiveFormat::ZipPassword),
                    excludes: self.archive_exclude_patterns(),
                    download: (!self.archive_download.is_empty())
                        .then(|| PathBuf::from(&self.archive_download)),
                    server_output: self.server_output(&authorize)?,
                    reuse_output: self.archive_server_only && self.archive_reuse_output,
                    member_layout: self.archive_member_layout,
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
                    replace_existing: self.privileged_replace_existing,
                    standing: self
                        .privileged_standing
                        .then(|| self.sync_standing_settings(true))
                        .transpose()?,
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
                    replace_changed: self.sync_replace_changed,
                    approval: self.sync_approval.clone(),
                    standing: self
                        .sync_standing
                        .then(|| self.sync_standing_settings(false))
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
                let mut transfer = self.rsync_transfer()?;
                if let Some(approval) = self.rsync_approval.as_ref().filter(|approval| {
                    self.rsync_candidate()
                        .is_some_and(|request| request.matches(approval))
                }) {
                    approval.apply(&mut transfer);
                }
                if self.rsync_import.is_some() {
                    if !self.rsync_import_matches(&transfer) {
                        return Err(
                            "Keep one native path and the directory placement of each imported Java rsync source.".into(),
                        );
                    }
                    let approved = transfer.approval.as_ref().is_some_and(|approval| {
                        self.source.is_rsync_upload()
                            || transfer.directory
                            || !transfer.additional_sources.is_empty()
                            || approval.download.is_some()
                    });
                    if !approved {
                        return Err(imported_transfer::RSYNC_REVIEW_REQUIRED.into());
                    }
                }
                action.remote = Some(if self.source.is_rsync_upload() {
                    RemoteAction::RsyncUpload(transfer)
                } else {
                    RemoteAction::RsyncDownload(transfer)
                });
            }
            Source::AgentDiagnostics | Source::SwarmDiagnostics => {
                if let Some(imported) = &self.imported_ai
                    && self.agent_profile.is_none()
                {
                    let mut imported = imported.as_ref().clone();
                    imported.goal = self.agent_goal.text();
                    imported.working_directory = action.working_directory.take();
                    if imported.kind == super::agent::Kind::Swarm {
                        imported.parallelism = self
                            .swarm_parallelism
                            .trim()
                            .parse()
                            .map_err(|_| "Choose a Swarm target parallelism from 1 to 16.")?;
                    }
                    action.imported_ai = Some(imported);
                } else {
                    action.agent = Some(super::agent::Action {
                        kind: if self.source == Source::AgentDiagnostics {
                            super::agent::Kind::Agent
                        } else {
                            super::agent::Kind::Swarm
                        },
                        probe: self.agent_probe,
                        profile: self.agent_profile.clone().ok_or("Choose an AI profile.")?,
                        goal: self.agent_goal.text(),
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
        }
        if self.sudo != SudoMode::Disabled {
            let metadata = match action.remote.as_ref() {
                Some(RemoteAction::Mkdir { path }) => Some((path, ProtectedOperation::Mkdir)),
                Some(RemoteAction::Chmod { path, mode }) => {
                    Some((path, ProtectedOperation::Chmod { mode: *mode }))
                }
                Some(RemoteAction::ChmodSymbolic { path, mode }) => Some((
                    path,
                    ProtectedOperation::ChmodSymbolic { mode: mode.clone() },
                )),
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
            enabled: self.enabled && !self.requires_import_review(),
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
        if let Some((copy, privileged)) = scheduled_copy
            && (!privileged || definition.enabled)
        {
            copy.validate_job_schedule(
                definition.schedule.as_ref(),
                definition.mode,
                &definition.dependencies,
                privileged,
            )?;
        }
        if action
            .remote
            .as_ref()
            .and_then(RemoteAction::rsync_transfer)
            .is_some_and(|(transfer, _)| transfer.standing.is_some())
        {
            self.rsync_standing_intent()?;
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
        connection_groups: &'a [super::ConnectionGroup],
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
            checkbox(self.enabled && !self.requires_import_review())
                .label("Enabled")
                .on_toggle_maybe((!self.requires_import_review()).then_some(Message::Enabled)),
            form_row("Name", input(Field::Name, "Job name", &self.name)),
            form_row("Connection", self.connection_view(connections)),
            (self.imported_route().is_none() && !self.has_imported_privileged()
                && (!self.has_unresolved_imported_ai() || self.has_connection_groups())).then(|| {
                form_row("Connection groups", self.connection_groups_view(connection_groups))
            }),
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
        summary_profiles: Vec<super::AutomationSummaryProvider>,
        permissions: ActionPermissions,
    ) -> Element<'a, Message> {
        if let Some(imported) = &self.imported_archive {
            return self.imported_archive_view(imported, archive_passwords);
        }
        if let Some(imported) = &self.imported_transfer {
            return self.imported_transfer_view(imported, permissions.privileged);
        }
        if let Some(imported) = &self.imported_privileged {
            return self.imported_privileged_view(imported, snippets, permissions.privileged);
        }
        let ActionPermissions {
            browse: can_browse,
            privileged: privileged_allowed,
            automation_journal,
            journal_output,
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
                if self.can_replace_privileged_file() {
                    fields = fields.push(
                        checkbox(self.privileged_replace_existing)
                            .label(if self.source == Source::RemoteUpload {
                                "Review replacement of an existing server file"
                            } else {
                                "Review replacement of an existing local file"
                            })
                            .on_toggle(Message::PrivilegedReplaceExisting),
                    );
                }
                if self.privileged_replace_existing {
                    fields = fields.push(
                        checkbox(self.privileged_standing)
                            .label("Allow recurring automatic replacements on this device")
                            .on_toggle(Message::PrivilegedStanding),
                    );
                    if self.privileged_standing {
                        fields = fields.push(self.standing_limits_view(true));
                    }
                }
                fields = fields
                    .push(text(if self.privileged_replace_existing && self.source == Source::RemoteUpload {
                        "Requires the Linux server helper with configured replacement roots. Both files can be up to 1 GiB."
                    } else if self.privileged_replace_existing {
                        "Requires one saved Linux SSH route and verified host keys. Each replacement retains the original local file."
                    } else {
                        "Requires one saved Linux SSH route, verified host keys and a new destination for one regular file."
                    }).size(12))
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
                    text(if self.source == Source::PrivilegedTreeDownload && self.sync_replace_changed {
                        "Requires a pinned Linux SSH route, a protected source and an existing local folder. Review missing entries and changed regular files before publishing."
                    } else if self.source == Source::PrivilegedTreeDownload {
                        "Requires a pinned Linux SSH route, a protected server folder, and an absent local destination. Reviews the entire tree up to 128 entries and 1 GiB."
                    } else if self.sync_replace_changed {
                        "Requires a pinned Linux SSH route and the installed privileged folder helper. Review missing entries and changed files within the configured server roots."
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
                fields = fields.push(form_row("Archive execution", self.sudo_view()));
                if self.sudo != SudoMode::Disabled {
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
                super::view::journal_detail_view(
                    self.journal_detail,
                    self.virtual_terminal.enabled,
                    journal_output,
                ),
            ]
            .spacing(8)
            .into()
        };
        column![
            self.source_view(
                snippets,
                archive_passwords,
                agent_profiles,
                can_browse,
                privileged_allowed
            ),
            capture,
            self.supports_automation_journal()
                .then(|| self.automation_journal_view(automation_journal, summary_profiles)),
            form_row(
                "Outcome notifications",
                self.desktop_notices.view(!self.webhook_targets.is_empty()),
            ),
            (!self.has_unresolved_imported_ai())
                .then(|| { form_row("Webhooks", self.webhook_view(webhook_choices)) }),
        ]
        .spacing(8)
        .padding(8)
        .into()
    }

    fn automation_journal_view(
        &self,
        allowed: bool,
        profiles: Vec<super::AutomationSummaryProvider>,
    ) -> Element<'_, Message> {
        if !allowed {
            return form_row(
                "Automation journal",
                text("Application policy disables automation journal capture.").size(12),
            );
        }
        let virtual_terminal =
            !self.source.is_remote() && !self.source.is_agent() && self.virtual_terminal.enabled;
        form_row(
            "Automation journal",
            column![
                self.automation_journal
                    .view(self.source.is_remote(), virtual_terminal, profiles)
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
        if let Some(imported) = &self.imported_privileged {
            return imported
                .routes
                .iter()
                .fold(column![].spacing(4), |rows, route| {
                    rows.push(text(route_label(route)))
                })
                .into();
        }
        if let Some(route) = self.imported_route() {
            return text(route_label(route)).into();
        }
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
                    "Targets run in the listed order. Rsync downloads use separate destinations. Virtual terminal recordings and ordinary SFTP downloads require one target."
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

    fn connection_groups_view<'a>(
        &'a self,
        groups: &'a [super::ConnectionGroup],
    ) -> Element<'a, Message> {
        let mut fields = column![].spacing(6);
        if !groups.is_empty() && !self.has_unresolved_imported_ai() {
            fields = fields.push(
                pick_list(
                    groups,
                    None::<super::ConnectionGroup>,
                    Message::ConnectionGroupAdded,
                )
                .placeholder("Add a saved connection group"),
            );
        }
        if let Some(source) = &self.target_groups {
            for group in &source.groups {
                fields = fields.push(
                    row![
                        text(group.to_string()).width(Length::Fill),
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

    fn imported_archive_view<'a>(
        &'a self,
        imported: &'a imported_archive::Draft,
        passwords: &'a [CredentialChoice],
    ) -> Element<'a, Message> {
        let settings = &imported.settings;
        let server_paths = settings.member_layout() == ArchiveMemberLayout::ServerPath;
        let source_paths = settings
            .sources
            .iter()
            .map(RemotePath::as_str)
            .collect::<Vec<_>>()
            .join("\n");
        let mut fields = column![
            text("Imported Java archive").size(16),
            form_row("SSH target", text(route_label(&imported.route))),
            form_row("Remote sources", text(source_paths).wrapping(text::Wrapping::Glyph)),
            form_row("Remote destination", text(settings.target.as_str()).wrapping(text::Wrapping::Glyph)),
            form_row("Archive format", text(if settings.format == ArchiveFormat::Tar {
                settings.format.to_string()
            } else {
                format!("{} · compression {}", settings.format, settings.level)
            })),
            text(if server_paths {
                "The adopted archive preserves complete server paths, as in Java. For example, /etc/hosts is stored as etc/hosts."
            } else {
                "Archive entries start with each source's file or folder name."
            }).size(12),
            text("Adoption keeps the job disabled. Existing archive paths require a separate reuse selection.").size(12),
        ].spacing(8);
        if settings.java_sudo {
            fields = fields.push(text("Java requested sudo. This job needs the installed Rust archive helper and a new review of the selected sudo mode and SSH route.").size(12));
        }
        if !settings.java_excludes.is_empty() {
            fields = fields.push(form_row(
                "Java exclude patterns",
                text(settings.java_excludes.join("\n")).wrapping(text::Wrapping::Glyph),
            ));
        }
        fields = fields
            .push(form_row("Rust exclude patterns", text_editor(&self.archive_excludes).placeholder("One optional exclude glob per line").height(Length::Fixed(90.0)).on_action(Message::EditArchiveExcludes)))
            .push(text(if server_paths {
                java_archive_exclude_help(settings.format)
            } else {
                "Review the exclusions for this archive. Relative globs start with the source name. A leading / matches the absolute server path. TAR also checks from each directory boundary."
            }).size(12));
        if let Some(local) = &settings.local_hint {
            fields = fields
                .push(form_row(
                    "Java download path",
                    text(local).wrapping(text::Wrapping::Glyph),
                ))
                .push(form_row(
                    "Local archive path",
                    input(
                        Field::ArchiveDownload,
                        "Enter a new absolute path on this device",
                        &self.archive_download,
                    ),
                ));
            if settings.java_sudo {
                fields = fields.push(text("The server administrator must allow archive copies from the remote destination.").size(12));
            }
        }
        if settings.format == ArchiveFormat::ZipPassword {
            fields = fields
                .push(self.archive_password_picker(passwords))
                .push(text("Select an archive password from the Rust vault. AES-256 protects file contents. File names remain visible.").size(12));
        }
        let adopt = if settings.java_sudo {
            "Adopt privileged archive"
        } else {
            "Adopt reviewed archive"
        };
        fields
            .push(button(adopt).on_press(Message::AdoptImportedArchive))
            .into()
    }

    fn archive_password_picker<'a>(
        &'a self,
        passwords: &'a [CredentialChoice],
    ) -> Element<'a, Message> {
        let choice = passwords
            .iter()
            .find(|choice| Some(choice.id.as_str()) == self.archive_password.as_deref());
        form_row(
            "Archive password",
            row![
                pick_list(passwords, choice, Message::ArchivePassword).placeholder(
                    if self.archive_password.is_some() {
                        "Saved password reference"
                    } else {
                        "Choose a ZIP archive password"
                    }
                ),
                button("Open Vault…").on_press(Message::OpenCredentialVault),
            ]
            .spacing(8),
        )
    }

    fn imported_privileged_view<'a>(
        &'a self,
        imported: &'a imported_privileged::Draft,
        snippets: &'a [SnippetChoice],
        privileged_allowed: bool,
    ) -> Element<'a, Message> {
        use imported_privileged::{FileAction, Settings};

        let mut fields = column![text("Imported Java sudo action").size(17)].spacing(8);
        fields = match &imported.settings {
            Settings::Command { text: command, .. } => fields.push(form_row(
                "Command",
                scrollable(text(command.as_str()).font(iced::Font::MONOSPACE))
                    .height(128)
                    .width(Length::Fill),
            )),
            Settings::Snippet { link, .. } => {
                let snippet = snippets.iter().find(|choice| choice.id == link.id);
                let fields = fields.push(form_row(
                    "Shared snippet",
                    text(
                        snippet
                            .map_or("Linked snippet unavailable", |choice| choice.label.as_str()),
                    ),
                ));
                if link.arguments.is_empty() {
                    fields
                } else {
                    fields.push(form_row(
                        "Arguments",
                        scrollable(text(link.arguments.join("\n")).font(iced::Font::MONOSPACE))
                            .height(96)
                            .width(Length::Fill),
                    ))
                }
            }
            Settings::File { action } => {
                let mut fields = action.fields().into_iter().fold(
                    fields.push(form_row("File action", text(action.label()))),
                    |rows, (label, value)| {
                        rows.push(form_row(label, text(value).wrapping(text::Wrapping::Glyph)))
                    },
                );
                let note = match action {
                    FileAction::Chown { .. } => Some(
                        "Review names on the server or enter the numeric IDs in the native action.",
                    ),
                    FileAction::Copy { .. } => {
                        Some("Choose a file or folder source in the native action.")
                    }
                    FileAction::Delete { .. } => {
                        Some("Repeat the absolute server path before reviewing recursive deletion.")
                    }
                    _ => None,
                };
                if let Some(note) = note {
                    fields = fields.push(text(note).size(12));
                }
                fields
            }
        };
        if let Some(options) = imported.settings.shell_options() {
            if let Some(path) = &options.working_directory {
                fields = fields.push(form_row("Working directory", text(path.as_str())));
            }
            if let Some(terminal) = options.virtual_terminal {
                fields = fields.push(form_row(
                    "Virtual terminal",
                    text(format!(
                        "{} × {} · Passwordless sudo",
                        terminal.columns, terminal.rows
                    )),
                ));
                if self.imported_sudo_mode == Some(SudoMode::Vault) {
                    fields = fields
                        .push(text("Vault-backed sudo uses a non-PTY SSH exec channel.").size(12));
                }
            }
        }
        if privileged_allowed {
            fields = fields.push(form_row(
                "Sudo",
                pick_list(
                    &SudoMode::ALL[1..],
                    self.imported_sudo_mode,
                    Message::ImportedPrivilegedSudo,
                )
                .placeholder("Choose a sudo mode"),
            ));
        } else {
            fields = fields
                .push(text("Application policy forbids this Scheduler sudo action.").size(12));
        }
        fields
            .push(
                button("Use native sudo action").on_press_maybe(
                    (privileged_allowed && self.imported_sudo_mode.is_some())
                        .then_some(Message::AdoptImportedPrivileged),
                ),
            )
            .into()
    }

    fn imported_transfer_view<'a>(
        &'a self,
        imported: &'a imported_transfer::Draft,
        privileged_allowed: bool,
    ) -> Element<'a, Message> {
        use imported_transfer::{Draft, Mode};

        let imported = match imported {
            Draft::Sftp(imported) => imported,
            Draft::StagedSftp(staged) => {
                return self.imported_staging_view(staged, privileged_allowed);
            }
            Draft::Rsync(imported) => {
                return self.imported_rsync_view(imported, privileged_allowed);
            }
        };

        let target = self
            .connection
            .as_ref()
            .filter(|_| self.additional_connections.is_empty());
        let ready = target.is_some() && !self.local_file.is_empty();
        column![
            text(imported.kind.label()).size(16),
            form_row("Path from Java", text(&imported.local_hint).wrapping(text::Wrapping::Glyph)),
            form_row("Remote path", text(imported.remote.as_str()).wrapping(text::Wrapping::Glyph)),
            form_row("SSH target", text(target.map_or_else(
                || "Choose one saved SSH target on the Job page.".into(),
                route_label,
            ))),
            form_row("Path on this device", input(Field::LocalFile, "Enter an absolute local path", &self.local_file)),
            text("Choose a native transfer. File and folder transfers require a new destination. A folder comparison needs a separate review and starts with automatic runs turned off.").size(12),
            row![
                button("Use regular file").on_press_maybe(ready.then_some(Message::AdoptImportedTransfer(Mode::File))),
                button("Use new folder").on_press_maybe(ready.then_some(Message::AdoptImportedTransfer(Mode::Folder))),
                button("Compare folders").on_press_maybe(ready.then_some(Message::AdoptImportedTransfer(Mode::Compare))),
            ].spacing(6).wrap(),
        ].spacing(8).into()
    }

    fn imported_staging_view<'a>(
        &'a self,
        staged: &'a imported_transfer::StagedSftpDraft,
        privileged_allowed: bool,
    ) -> Element<'a, Message> {
        use imported_transfer::{Mode, StagedDestination};

        let ready = privileged_allowed
            && self.imported_sudo_mode.is_some()
            && !self.local_file.is_empty()
            && !self.remote_path.is_empty();
        let adopt = |destination| {
            ready.then_some(Message::AdoptImportedTransfer(Mode::Staged(destination)))
        };
        column![
            text(staged.settings.kind.label()).size(16),
            form_row(
                "Path from Java",
                text(&staged.settings.local_hint).wrapping(text::Wrapping::Glyph)
            ),
            form_row(
                "Server path from Java",
                text(&staged.settings.remote_hint).wrapping(text::Wrapping::Glyph)
            ),
            form_row("SSH target", text(route_label(&staged.route))),
            form_row(
                "Local path",
                input(
                    Field::LocalFile,
                    "Absolute path on this device",
                    &self.local_file
                )
            ),
            form_row(
                "Server path",
                input(Field::RemotePath, "Absolute Linux path", &self.remote_path)
            ),
            if privileged_allowed {
                form_row(
                    "Privileged staging",
                    pick_list(
                        [SudoMode::Passwordless, SudoMode::Vault],
                        self.imported_sudo_mode,
                        Message::ImportedStagingSudo,
                    )
                    .placeholder("Choose sudo authorization"),
                )
            } else {
                text("Application policy forbids privileged staging.")
                    .size(12)
                    .into()
            },
            row![
                button("Use new file").on_press_maybe(adopt(StagedDestination::NewFile)),
                button("Replace existing file")
                    .on_press_maybe(adopt(StagedDestination::ReplaceFile)),
                button("Use new folder").on_press_maybe(adopt(StagedDestination::NewFolder)),
                button(
                    if staged.settings.kind.direction() == imported_transfer::Direction::Download {
                        "Use existing local folder"
                    } else {
                        "Use existing server folder"
                    }
                )
                .on_press_maybe(adopt(StagedDestination::ExistingFolder)),
            ]
            .spacing(6)
            .wrap(),
        ]
        .spacing(8)
        .into()
    }

    fn imported_rsync_view<'a>(
        &'a self,
        imported: &'a imported_transfer::RsyncDraft,
        privileged_allowed: bool,
    ) -> Element<'a, Message> {
        if imported.java_sudo {
            let mixed = imported.mixed_settings(super::rsync::Elevation::Passwordless);
            let choices = [
                (false, false, "Configure passwordless file transfer"),
                (false, true, "Configure passwordless existing file"),
                (true, false, "Configure passwordless new folder"),
                (true, true, "Configure passwordless existing folder"),
            ];
            return column![
                Self::imported_rsync_paths(imported),
                text(
                    "Passwordless sudo requires the configured Linux helper on every saved route."
                )
                .size(13),
                mixed.as_ref().err().map(|error| text(*error).size(13)),
                mixed
                    .is_ok()
                    .then(
                        || button("Configure passwordless mixed transfer").on_press_maybe(
                            privileged_allowed
                                .then_some(Message::ConfigureImportedPasswordlessMixedRsync,)
                        )
                    ),
                choices
                    .into_iter()
                    .fold(
                        row![].spacing(6),
                        |choices, (directory, existing, label)| {
                            choices.push(
                                imported
                                    .passwordless_settings(directory, existing)
                                    .is_ok()
                                    .then(|| {
                                        button(label).on_press_maybe(privileged_allowed.then_some(
                                            Message::ConfigureImportedPasswordlessRsync {
                                                directory,
                                                existing,
                                            },
                                        ))
                                    }),
                            )
                        }
                    )
                    .wrap(),
            ]
            .spacing(8)
            .into();
        }
        let ready: Element<'a, Message> = match imported.require_native_scope() {
            Ok(()) => row![
                button("Configure new-file transfer")
                    .on_press(Message::ConfigureImportedRsync { directory: false }),
                button("Configure new-folder transfer")
                    .on_press(Message::ConfigureImportedRsync { directory: true }),
            ]
            .spacing(6)
            .wrap()
            .into(),
            Err(reason) => text(reason).size(13).into(),
        };
        column![
            Self::imported_rsync_paths(imported),
            text("Choose the native destination behavior and review every server route before saving.").size(13),
            imported.mixed_settings(super::rsync::Elevation::Ordinary).is_ok().then(|| button("Configure mixed source transfer")
                .on_press(Message::ConfigureImportedMixedRsync)),
            imported.existing_settings().is_ok().then(|| button(match imported.direction {
                imported_transfer::Direction::Upload => "Configure existing server folder",
                imported_transfer::Direction::Download => "Configure existing local folder",
            }).on_press(Message::ConfigureImportedExistingRsync)),
            imported.existing_local_file_source().is_ok().then(|| button("Configure existing local file")
                .on_press(Message::ConfigureImportedExistingRsyncFile)),
            ready,
        ]
        .spacing(8)
        .into()
    }

    fn imported_rsync_paths(
        imported: &imported_transfer::RsyncDraft,
    ) -> iced::widget::Column<'_, Message> {
        let label = match imported.direction {
            imported_transfer::Direction::Upload => "Java rsync upload",
            imported_transfer::Direction::Download => "Java rsync download",
        };
        column![
            text(label).size(16),
            form_row(
                "Java sources",
                text(imported.java_sources.join("\n")).wrapping(text::Wrapping::Glyph)
            ),
            form_row(
                "Java destination root",
                text(&imported.java_target_root).wrapping(text::Wrapping::Glyph)
            ),
        ]
        .spacing(8)
        .push(
            imported
                .java_sudo
                .then(|| text("Java option · Use sudo").size(13)),
        )
        .push(
            imported
                .java_delete
                .then(|| text("Java option · Delete unmatched entries").size(13)),
        )
    }

    fn source_view<'a>(
        &'a self,
        snippets: &'a [SnippetChoice],
        archive_passwords: &'a [CredentialChoice],
        agent_profiles: &'a super::agent::ProfileChoices,
        can_browse: bool,
        privileged_allowed: bool,
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
            editor = editor.push(form_row(
                "AI profile",
                pick_list(
                    agent_profiles.entries.as_slice(),
                    profile,
                    Message::AgentProfile,
                )
                .placeholder(placeholder),
            ));
            if let Some(imported) = &self.imported_ai {
                editor = editor.push(text("This imported job stays disabled when saved. Review the native workflow before enabling it.").size(12));
                if self.has_unresolved_imported_ai() {
                    if let Some(suggestion) = imported
                        .profile_source
                        .as_ref()
                        .and_then(|source| agent_profiles.for_java_source(source))
                    {
                        editor = editor.push(
                            row![
                                text(format!("Imported profile · {suggestion}")).size(12),
                                button("Use imported profile")
                                    .on_press(Message::AgentProfile(suggestion.clone())),
                            ]
                            .spacing(8)
                            .wrap(),
                        );
                    } else {
                        editor = editor.push(text("Review the imported AI profile in AI Manager when available, or choose a profile here.").size(12));
                    }
                }
            }
            editor = editor.push(if self.can_edit_agent_workflow() {
                form_row(
                    "Workflow",
                    pick_list(workflows, workflow, Message::AgentProbe),
                )
            } else {
                form_row("Workflow", text(self.agent_probe.to_string()))
            });
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
                .push(form_row("Question", text_editor(&self.agent_goal)
                    .placeholder("What should this workflow address?")
                    .height(Length::Fixed(96.0))
                    .on_action(Message::EditAgentGoal)))
                .push(text("Set an absolute SSH working directory in the Job tab. Review the selected profile and host keys before running.").size(12))
                .push(button("Review AI workflow…").on_press_maybe((!self.has_unresolved_imported_ai()).then_some(Message::ReviewAgent)))
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
            let protected_file =
                self.source == Source::RsyncUpload && self.rsync_existing.is_some();
            let browse_remote = can_browse
                && (!self.source.is_rsync()
                    || self.rsync_elevation != super::rsync::Elevation::Passwordless);
            let rsync_batch = self.source.is_rsync()
                && !self.rsync_mixed
                && !self.rsync_sources.text().trim().is_empty();
            let grouped_rsync = self.source.is_rsync() && !self.additional_connections.is_empty();
            editor = editor.push(
                (!self.rsync_mixed || self.source.is_rsync_upload()).then(|| {
                    form_row(
                        if self.source.is_rsync_upload() {
                            if grouped_rsync {
                                "Primary server destination"
                            } else if self.rsync_allow_vacant_server_file_upload {
                                "Server destination file"
                            } else if protected_file {
                                "Existing server file"
                            } else if rsync_batch || self.rsync_existing.is_some() {
                                "Server destination folder"
                            } else {
                                "Server destination"
                            }
                        } else if rsync_batch {
                            "First server source"
                        } else {
                            "Remote path"
                        },
                        row![
                            input(
                                Field::RemotePath,
                                if self.source == Source::RemoteArchive && self.archive_reuse_output
                                {
                                    "Absolute archive path on the SSH server"
                                } else if self.source == Source::RemoteArchive {
                                    "Absolute new archive path on the SSH server"
                                } else if self.rsync_allow_vacant_server_file_upload {
                                    "Absolute file path on the SSH server"
                                } else if protected_file {
                                    "Absolute existing file path on the SSH server"
                                } else {
                                    "Absolute path on the SSH server"
                                },
                                &self.remote_path,
                            ),
                            self.browse_directory_button(DirectoryField::RemotePath, browse_remote),
                        ]
                        .spacing(6),
                    )
                }),
            );
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
                Source::RemoteChmod => editor
                    .push(form_row("Permissions", input(
                        Field::RemoteMode,
                        "Octal or symbolic mode (e.g. 0755 or u+rw,o-w)",
                        &self.remote_mode,
                    )))
                    .push(text(if self.sudo == SudoMode::Disabled {
                        "Symbolic modes require explicit u, g, o or a and an OpenSSH server with lsetstat support."
                    } else {
                        "Symbolic modes require explicit u, g, o or a. Privileged chmod changes ordinary permission bits only."
                    }).size(12)),
                Source::RemoteChown => editor
                    .push(form_row("Owner", self.owner_id_entry(AccountKind::User, can_browse)))
                    .push(form_row("Group", self.owner_id_entry(AccountKind::Group, can_browse)))
                    .push((can_browse && self.ownership_target().is_none()).then(|| {
                        text("Name lookup requires one saved SSH connection with verified host keys.")
                            .size(12)
                    }))
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
                    let mut archive = editor
                        .push(checkbox(self.archive_server_only).label("Create entirely on the server").on_toggle(Message::ArchiveServerOnly))
                        .push(form_row("Archive format", pick_list(self.archive_formats(), Some(self.archive_format), Message::ArchiveFormat)))
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
                        archive = archive
                            .push(self.archive_password_picker(archive_passwords))
                            .push(text("The vault must be unlocked when the job starts. AES-256 protects file contents. File names remain visible.").size(12));
                    }
                    archive = archive.push(form_row("Local archive path", input(Field::ArchiveDownload, "Optional absolute local archive path", &self.archive_download)));
                    let server_paths = self.archive_member_layout == ArchiveMemberLayout::ServerPath;
                    if self.archive_server_only {
                        if self.archive_format.supports_server_member_paths() {
                            archive = archive.push(checkbox(server_paths).label("Keep complete server paths in archive entries (Java layout)").on_toggle(Message::ArchiveServerPath));
                        }
                        archive = archive.push(checkbox(self.archive_reuse_output).label("Reuse reviewed archive paths").on_toggle(Message::ArchiveReuseOutput));
                        if self.archive_reuse_output {
                            archive = archive.push(text("Archive reuse requires the Linux helper. ZIP keeps unmatched entries. TAR, TAR.BZ2 and 7z are rebuilt from the current sources. Previous files are kept for recovery.").size(12));
                        }
                    }
                    if self.server_output_enabled() {
                        archive = archive
                            .push(form_row("Output owner", self.owner_id_entry(AccountKind::User, can_browse)))
                            .push(form_row("Output group", self.owner_id_entry(AccountKind::Group, can_browse)))
                            .push(form_row("Output permissions", input(Field::ArchiveMode, "Three octal digits, defaults to 600", &self.archive_mode)))
                            .push(text("A local copy requires file-transfer permission. The server archive is kept if the local copy fails.").size(12));
                    }
                    if self.privileged_archive() && self.sudo != SudoMode::Disabled {
                        archive = archive.push(text(if self.archive_reuse_output {
                            "A local copy requires file-transfer permission and a server path within a configured protected download root. Reuse also checks any existing local copy before replacing it."
                        } else {
                            "A local copy requires file-transfer permission, a server path within a configured protected download root and a new local path."
                        }).size(12));
                    }
                    archive
                        .push(text(if server_paths {
                            java_archive_exclude_help(self.archive_format)
                        } else {
                            "Glob paths start with the source name. A leading / matches the absolute remote path. TAR also checks from each directory boundary."
                        }).size(12))
                        .push(text(if self.privileged_archive() && self.sudo != SudoMode::Disabled && self.archive_reuse_output {
                            "Creates or updates a root-owned archive with private permissions. Each run checks the reviewed output paths and verified SSH route."
                        } else if self.privileged_archive() && self.sudo != SudoMode::Disabled {
                            "Creates a new root-owned archive with private permissions using one saved Linux SSH connection and verified host keys."
                        } else if self.archive_server_only && self.archive_reuse_output {
                            "Rechecks the saved output paths and SSH route before each publication."
                        } else if self.archive_server_only {
                            "Creates an archive at a new path with the saved output attributes using the server archive helper and one verified SSH route."
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
                    .push(form_row("Local destination", input(Field::LocalFile,
                        if self.sync_replace_changed { "Absolute existing local folder" } else { "Absolute new local folder" }, &self.local_file)))
                    .push(checkbox(self.sync_replace_changed)
                        .label("Review changes in an existing local folder")
                        .on_toggle(Message::SyncReplaceChanged))
                    .push(self.protected_tree_standing_view()),
                Source::SyncUpload | Source::SyncDownload | Source::PrivilegedTreeUpload => {
                    let editor = editor
                        .push(form_row("Local folder", input(Field::LocalFile, "Absolute existing local folder", &self.local_file)))
                        .push(form_row("Excludes", text_editor(&self.sync_excludes)
                            .placeholder("One optional relative glob per line")
                            .height(Length::Fixed(80.0))
                            .on_action(Message::EditSyncExcludes)));
                    if self.privileged_tree() {
                        editor
                            .push(checkbox(self.sync_replace_changed)
                                .label("Review changes in an existing server folder")
                                .on_toggle(Message::SyncReplaceChanged))
                            .push(self.protected_tree_standing_view())
                    } else {
                        let editor = editor.push(checkbox(self.sync_replace_changed)
                            .label(if self.source == Source::SyncDownload {
                                "Replace changed local files after review"
                            } else {
                                "Replace changed server files after review"
                            })
                            .on_toggle(Message::SyncReplaceChanged));
                        let editor = editor.push(
                            checkbox(self.sync_standing)
                                .label("Allow recurring automatic copies on this device")
                                .on_toggle(Message::SyncStanding),
                        );
                        let editor = if self.sync_standing {
                            editor
                                .push(self.standing_limits_view(false))
                                .push(text(if self.sync_replace_changed {
                                    "Each calendar firing can replace eligible changed files and copy missing entries within these limits. The original files stay in private recovery folders."
                                } else {
                                    "Each approved firing can copy missing files and folders within these limits. Calendar and Chain starts share the firing allowance."
                                }).size(12))
                        } else if self.sync_replace_changed {
                            editor.push(text("Compare regular file contents. Each replaced file retains its original in a private sibling folder. Save one approval for one run.").size(12))
                        } else {
                            editor.push(text("Copy only the reviewed missing files and folders. Save one approval for Run Now or one scheduled date and time.").size(12))
                        };
                        editor.push(button("Compare and review…").on_press(Message::ReviewSync))
                    }
                }
                Source::RsyncDownload | Source::RsyncDownloadTree | Source::RsyncUpload | Source::RsyncUploadTree => editor
                    .push(self.rsync_import.as_deref().map(Self::imported_rsync_paths))
                    .push((self.supports_rsync_existing() || self.supports_rsync_existing_file())
                        .then(|| self.rsync_existing_controls(rsync_batch, privileged_allowed)))
                    .push((!self.rsync_mixed || !self.source.is_rsync_upload()).then(|| if rsync_batch && self.source.is_rsync_upload() {
                        form_row("First local source", input(Field::LocalFile, "Absolute first local source path", &self.local_file))
                    } else if rsync_batch {
                        form_row(if grouped_rsync { "Primary local destination folder" } else { "Local destination folder" }, input(Field::LocalFile, "Absolute existing local folder", &self.local_file))
                    } else if self.source == Source::RsyncUpload {
                        form_row("Local source", input(Field::LocalFile, "Absolute regular local file path", &self.local_file))
                    } else if self.source == Source::RsyncUploadTree {
                        form_row("Local source", input(Field::LocalFile, "Absolute local folder path", &self.local_file))
                    } else if self.source == Source::RsyncDownloadTree {
                        form_row(if grouped_rsync { "Primary local destination" } else { "Local destination" }, input(Field::LocalFile,
                            if self.rsync_mixed { "Absolute local destination folder" }
                            else if self.rsync_allow_vacant_directory_download && self.rsync_existing.is_some_and(|settings| !settings.include_source_name) { "Absolute local folder path" }
                            else if self.rsync_existing.is_some() { "Absolute existing local folder" } else { "Absolute new local folder path" }, &self.local_file))
                    } else {
                        form_row(if grouped_rsync { "Primary local destination" } else { "Local destination" }, input(Field::LocalFile,
                            if self.rsync_existing.is_some() { "Absolute existing local file path" }
                            else if grouped_rsync && self.rsync_standing { "Absolute local file path" }
                            else { "Absolute new local file path" }, &self.local_file))
                    }))
                    .push(grouped_rsync.then(|| self.rsync_destination_fields(rsync_batch)))
                    .push((self.rsync_mixed || self.rsync_elevation == super::rsync::Elevation::Ordinary && self.rsync_existing.is_none() && !self.rsync_allow_vacant_file_download).then(|| form_row(if self.rsync_mixed { "Sources" } else { "Additional sources" }, text_editor(&self.rsync_sources)
                        .placeholder(if self.rsync_mixed { "Absolute file or folder paths, one per line" } else { "Optional absolute source paths, one per line" })
                        .height(Length::Fixed(74.0))
                        .on_action(Message::EditRsyncSources))))
                    .push(form_row("Rsync executable", input(Field::RsyncProgram, "Absolute path to the installed native rsync executable", &self.rsync_program)))
                    .push(text(match (cfg!(any(unix, windows)), self.source, self.rsync_mixed) {
                        (true, _, true) => "Use a trailing / to transfer a folder’s contents. Mixed uploads require the current kortty server helper. Each target is checked and published as one folder.",
                        (true, Source::RsyncDownloadTree, false) => "Download ordinary directories. Requires native rsync 3.4.1 or later in the rsync 3 series and pinned saved SSH routes.",
                        (true, Source::RsyncUploadTree, false) => "Upload reviewed directories. Requires native rsync 3.4.1 or later, pinned saved SSH routes, and the current kortty server helper on Linux or macOS.",
                        (true, Source::RsyncUpload, false) if self.rsync_allow_vacant_server_file_upload => "Review the exact server file on each route. The current Linux helper checks its parent and verifies creation and replacement.",
                        (true, Source::RsyncUpload, false) if protected_file => "Replace the reviewed protected file through the configured Linux helper. A successful run retains the original for recovery.",
                        (true, Source::RsyncDownload, false) if self.rsync_existing.is_some() => "Replace the reviewed local file. A successful run retains the original for recovery.",
                        (true, Source::RsyncDownload, false) if grouped_rsync && self.rsync_standing && !rsync_batch => "Review the exact local file path for each saved SSH route. A successful replacement retains the prior file for recovery.",
                        (true, Source::RsyncDownload, false) if self.rsync_allow_vacant_file_download => "Review the exact vacant file path and any parent folders the transfer will create.",
                        (true, _, false) => "Transfer regular files to vacant paths. Requires native rsync 3.4.1 or later in the rsync 3 series and pinned saved SSH routes.",
                        (false, _, _) => "Scheduled rsync is unavailable on this platform.",
                    }).size(12))
                    .push(cfg!(windows).then(|| text("Choose an installed Windows rsync executable, such as rsync.exe from MSYS2. The transfer uses the saved SSH route.").size(12)))
                    .push((self.rsync_elevation == super::rsync::Elevation::Ordinary && self.rsync_existing.is_none() && !self.rsync_allow_vacant_file_download).then(|| text("For multiple sources, enter an existing destination folder. Each source keeps its name. Limits are 16 sources, 4,096 entries and 256 MiB total. A later failure can leave already completed items in place.").size(12)))
                    .push(self.rsync_standing_controls(rsync_batch, privileged_allowed))
                    .push(self.rsync_passwordless_controls(privileged_allowed))
                    .push(button("Review rsync transfer…").on_press_maybe(
                        (cfg!(any(unix, windows)) && (self.rsync_elevation != super::rsync::Elevation::Passwordless || privileged_allowed))
                            .then_some(Message::ReviewRsyncProgram)
                    )),
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

    fn rsync_standing_controls(
        &self,
        batch: bool,
        privileged_allowed: bool,
    ) -> iced::widget::Column<'_, Message> {
        let vacant_file = self.supports_vacant_rsync_file();
        let ordinary_file = self.supports_ordinary_rsync_server_file();
        let supported = self.rsync_existing.is_some()
            || self.rsync_mixed
            || ((vacant_file || ordinary_file) && !batch);
        let elevated = self.rsync_elevation == super::rsync::Elevation::Passwordless;
        let mut controls = column![].spacing(6);
        if supported || self.rsync_standing {
            controls = controls.push(
                checkbox(self.rsync_standing)
                    .label("Approve recurring rsync")
                    .on_toggle_maybe(
                        (!elevated || privileged_allowed || self.rsync_standing)
                            .then_some(Message::RsyncStanding),
                    ),
            );
        }
        if self.rsync_standing {
            controls = controls
                .push(text("Set a calendar or choose Chain with predecessors.").size(12))
                .push(self.supports_vacant_rsync_server_file().then(|| {
                    checkbox(self.rsync_allow_vacant_server_file_upload)
                        .label("Allow creation at reviewed vacant server file paths")
                        .on_toggle_maybe(
                            (!elevated || privileged_allowed || self.rsync_allow_vacant_server_file_upload)
                                .then_some(Message::RsyncAllowVacantServerFile),
                        )
                }))
                .push(self.supports_vacant_rsync_server_tree().then(|| {
                    checkbox(self.rsync_allow_vacant_server_tree_upload)
                        .label("Allow creation at reviewed vacant server folder paths")
                        .on_toggle_maybe(
                            (!elevated || privileged_allowed || self.rsync_allow_vacant_server_tree_upload)
                                .then_some(Message::RsyncAllowVacantServerTree),
                        )
                }))
                .push(text(match (vacant_file && !batch, self.additional_connections.is_empty()) {
                    (true, false) => "Choose one server file and a separate local path for each route. The review shows which files will be created or replaced and any parent folders to be created.",
                    (true, true) => "Choose one server file and a vacant local path. The review shows any parent folders to be created. For a file already on disk, select Replace an existing local file.",
                    (false, _) if self.rsync_mixed && self.source == Source::RsyncDownloadTree => "Choose a local folder path for each route. The review shows any parent folders to be created and where unmatched entries can be removed.",
                    (false, _) if self.rsync_allow_vacant_directory_download => "The review shows which destination folders will be created or replaced and where unmatched entries can be removed.",
                    (false, _) if self.rsync_allow_vacant_server_file_upload => "The review shows whether each route will create or replace its server file.",
                    (false, _) if self.rsync_allow_vacant_server_tree_upload => "The Linux helper checks creation and replacement permissions. The review shows what each route will create or replace.",
                    (false, _) if ordinary_file => "Allow creation at reviewed vacant server file paths to review the file on each route.",
                    (false, _) => "The review checks the destination on each route.",
                }).size(12))
                .push(form_row("Approval duration (days)", input(Field::RsyncStandingDays, "Days", &self.rsync_standing_days)))
                .push(form_row("Maximum firings", input(Field::RsyncStandingFirings, "Firings", &self.rsync_standing_firings)))
                .push(form_row("Entries per firing", input(Field::RsyncStandingEntries, "Maximum entries", &self.rsync_standing_entries)))
                .push(form_row("MiB per firing", input(Field::RsyncStandingMib, "Maximum MiB", &self.rsync_standing_mib)))
                .push(form_row("Deletions per firing", input(Field::RsyncStandingDeletes, "Maximum deletions", &self.rsync_standing_deletes)));
        }
        controls
    }

    fn rsync_passwordless_controls(
        &self,
        privileged_allowed: bool,
    ) -> iced::widget::Column<'_, Message> {
        let enabled = self.rsync_elevation == super::rsync::Elevation::Passwordless;
        let editable = self.rsync_import.is_none()
            && (enabled
                || privileged_allowed
                    && (self.rsync_mixed || self.rsync_sources.text().trim().is_empty()));
        column![
            checkbox(enabled)
                .label("Use passwordless sudo")
                .on_toggle_maybe(editable.then_some(Message::RsyncPasswordless)),
            enabled.then(|| text("Each saved route needs the configured Linux helper. Review the exact source and destination before running.").size(12)),
            (enabled && self.rsync_import.is_some() && self.rsync_existing.is_none()
                && matches!(self.source, Source::RsyncUploadTree | Source::RsyncDownloadTree))
                .then(|| text("For a named Java source, enter the full new destination path including the source directory name.").size(12)),
        ].spacing(6)
    }

    fn rsync_existing_controls(
        &self,
        batch: bool,
        privileged_allowed: bool,
    ) -> iced::widget::Column<'_, Message> {
        let editable = self.rsync_import.is_none();
        let settings = self.rsync_existing;
        if self.supports_rsync_existing_file() {
            let upload = self.source.is_rsync_upload();
            let elevated = self.rsync_elevation == super::rsync::Elevation::Passwordless;
            let allow_vacant_file = (self.supports_vacant_rsync_file() && !self.rsync_standing
                || self.rsync_allow_vacant_file_download)
                .then(|| {
                    checkbox(self.rsync_allow_vacant_file_download)
                        .label("Allow creation at reviewed vacant file paths")
                        .on_toggle_maybe(
                            (!elevated
                                || privileged_allowed
                                || self.rsync_allow_vacant_file_download)
                                .then_some(Message::RsyncAllowVacantFile),
                        )
                });
            return column![
                (!upload || elevated).then(|| checkbox(settings.is_some())
                    .label(if upload {
                        "Review the server file destination"
                    } else {
                        "Replace an existing local file"
                    })
                    .on_toggle_maybe(
                        (editable
                            && !batch
                            && (!elevated || privileged_allowed || settings.is_some()))
                        .then_some(Message::RsyncExisting)
                    )),
                allow_vacant_file,
            ]
            .spacing(6);
        }
        let upload = self.source.is_rsync_upload();
        let elevated = self.rsync_elevation == super::rsync::Elevation::Passwordless;
        let mixed = super::rsync::LOCAL_EXISTING_SUPPORTED.then(|| {
            checkbox(self.rsync_mixed)
                .label("Transfer mixed file and folder sources")
                .on_toggle_maybe(editable.then_some(Message::RsyncMixed))
        });
        let allow_vacant = self.supports_vacant_rsync_directory().then(|| {
            checkbox(self.rsync_allow_vacant_directory_download)
                .label("Allow creation at reviewed vacant paths")
                .on_toggle_maybe(
                    (!elevated || privileged_allowed || self.rsync_allow_vacant_directory_download)
                        .then_some(Message::RsyncAllowVacantDirectory),
                )
        });
        if self.rsync_mixed {
            return column![
                mixed,
                allow_vacant,
                checkbox(settings.is_some_and(|settings| settings.delete))
                    .label("Delete unmatched entries in synchronized folders")
                    .on_toggle_maybe(editable.then_some(Message::RsyncMirror)),
            ]
            .spacing(6);
        }
        column![
            mixed,
            checkbox(settings.is_some())
                .label(if upload && elevated { "Review the server destination folder" } else if upload { "Use an existing server folder" } else { "Review the local destination folder" })
                .on_toggle_maybe((editable && !batch).then_some(Message::RsyncExisting)),
            settings.map(|settings| column![
                allow_vacant,
                checkbox(settings.include_source_name)
                    .label(if upload { "Include the local folder name under each server destination" } else { "Include the server folder name under each local destination" })
                    .on_toggle_maybe(editable.then_some(Message::RsyncIncludeSourceName)),
                checkbox(settings.delete)
                    .label("Delete receiver entries missing from the source")
                    .on_toggle_maybe(editable.then_some(Message::RsyncMirror)),
                text(if upload && elevated && self.rsync_standing {
                    "The Linux helper verifies each new protected destination before the next run."
                } else if upload && elevated {
                    "Review the protected server folder again after each successful run."
                } else if upload {
                    "Requires the current kortty server helper and an ordinary Linux login. Review the server folder again after each successful run."
                } else if self.rsync_allow_vacant_directory_download {
                    "The review shows any missing parent folders the transfer will create."
                } else {
                    "Review the local folder again after each successful run."
                }).size(12),
            ].spacing(6)),
        ].spacing(6)
    }

    fn rsync_destination_fields(&self, batch: bool) -> iced::widget::Column<'_, Message> {
        let protected_file = self.supports_rsync_existing_file() && self.rsync_existing.is_some();
        let placeholder = match (
            self.rsync_mixed,
            self.source.is_rsync_upload(),
            batch || self.rsync_existing.is_some(),
        ) {
            (true, true, _) => "Absolute server destination folder",
            (true, false, _) => "Absolute local destination folder",
            (false, true, _) if self.rsync_allow_vacant_server_file_upload => {
                "Absolute server file path"
            }
            (false, true, _) if self.rsync_allow_vacant_server_tree_upload => {
                "Absolute server folder path"
            }
            (false, true, true) if protected_file => "Absolute existing server file",
            (false, true, true) => "Absolute existing server folder",
            (false, true, false) => "Absolute new server path",
            (false, false, true) if protected_file => "Absolute existing local file",
            (false, false, true)
                if self.rsync_allow_vacant_directory_download
                    && self
                        .rsync_existing
                        .is_some_and(|settings| !settings.include_source_name) =>
            {
                "Absolute local folder path"
            }
            (false, false, true) => "Absolute existing local folder",
            (false, false, false)
                if self.source == Source::RsyncDownload && self.rsync_standing =>
            {
                "Absolute local file path"
            }
            (false, false, false) => "Absolute new local path",
        };
        self.additional_connections.iter().fold(
            column![text("The same source paths apply to every target. Give each additional target its own destination.").size(12)].spacing(6),
            |fields, reference| {
                let id = reference.id;
                let destination = self.rsync_destinations.get(&id).map_or("", String::as_str);
                fields.push(column![
                    text(format!("Profile {id} · {}", route_label(reference))).size(13),
                    text_input(placeholder, destination)
                        .on_input(move |value| Message::RsyncDestination(id, value)),
                ].spacing(4))
            },
        )
    }
}

fn route_label(target: &ConnectionRef) -> String {
    target.jump.as_ref().map_or_else(
        || target.target.label(),
        |jump| format!("{} via {}", target.target.label(), jump.label()),
    )
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
    ImportedTransfer,
    ImportedPrivileged,
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
            Self::ImportedTransfer
                | Self::ImportedPrivileged
                | Self::RemoteMkdir
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

    fn is_rsync(self) -> bool {
        self.is_rsync_upload() || matches!(self, Self::RsyncDownload | Self::RsyncDownloadTree)
    }

    fn is_rsync_upload(self) -> bool {
        matches!(self, Self::RsyncUpload | Self::RsyncUploadTree)
    }

    pub(super) fn is_agent(self) -> bool {
        matches!(self, Self::AgentDiagnostics | Self::SwarmDiagnostics)
    }
}

impl std::fmt::Display for Source {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Command => "Command",
            Self::ImportedTransfer => "Imported Java SFTP transfer",
            Self::ImportedPrivileged => "Imported Java sudo action",
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
            Self::RsyncDownload => "rsync · Download file",
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

#[test]
fn java_group_rsync_adoption_prefills_route_layout_without_approval() {
    use super::rsync::Elevation;
    use serde_json::json;

    let profile = crate::snippets::tests::Profile::new();
    let root = profile.0.canonicalize().unwrap().join("Java copy\u{2003}");
    let first =
        json!({"id": 1, "target": {"host": "first.example", "port": 22, "username": "operator"}});
    let second =
        json!({"id": 2, "target": {"host": "second.example", "port": 22, "username": "operator"}});
    let command = serde_json::from_value(json!({
        "connection": first, "additional_connections": [second],
        "imported_transfer": {
            "direction": "download", "java_sources": ["/work/report"],
            "java_target_root": root, "java_delete": false, "java_sudo": false,
            "java_target_folders": { "1": "One-b04d413f", "2": "Two-9c0719d8" },
        },
    }))
    .unwrap();
    let job = Job::new(
        JobId::new("java-group-layout").unwrap(),
        "Java group",
        command,
    )
    .unwrap()
    .with_enabled(false);
    for (directory, existing, mixed, append_name) in [
        (false, false, false, true),
        (false, true, false, true),
        (true, false, false, true),
        (true, true, false, false),
        (true, true, true, false),
    ] {
        let mut draft = Draft::from_job(&job);
        draft
            .configure_imported_rsync(directory, existing, mixed, Elevation::Ordinary)
            .unwrap();
        assert!(draft.rsync_candidate().is_none());
        let transfer = draft.rsync_transfer().unwrap();
        let actual = [
            transfer.local,
            PathBuf::from(&transfer.routes[0].destination),
        ];
        let expected = ["One-b04d413f", "Two-9c0719d8"].map(|folder| {
            let path = root.join(folder);
            if append_name {
                path.join("report")
            } else {
                path
            }
        });
        assert_eq!(actual, expected);
        assert!(transfer.approval.is_none() && transfer.routes[0].approval.is_none());
        assert!(!root.exists());
    }
}
