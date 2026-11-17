use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use iced::Task;
use iced::widget::{pane_grid, text_editor};
use jiff::{Span, Timestamp, tz::OffsetConflict};
use kortty_jobs::{
    Blocker, FinishedRunMetadata, JobGraph, JobId, JournalRecording, MAX_JOURNAL_RECORDS, RunBatch,
    RunId, RunOutcome, RunRecord, RunRequest, RunTrigger, Scheduler, SchedulerStore, StoreError,
    SwarmChatOrigin, SwarmChatSeed, SwarmReport, Time, TimeZone, Weekday,
    valid_session_journal_ids,
};
use kortty_ssh::Endpoint;
use serde::{Deserialize, Serialize};

use crate::ai_manager::{Configuration, ProfileRef};
use crate::connection_store::Store as ConnectionStore;
use crate::connection_window::CredentialChoice;
use crate::diagram_window::{self, Format};
use crate::job_executor::{CapturedOutput, Completion, Credentials, virtual_terminal};
use crate::scheduled_agent_review::Ticket as ProposalTicket;
use crate::session_journal::automation::run_links::RunJournals;
use crate::snippets::{self, Library, SnippetId};

pub(crate) mod agent;
mod automation_journal;
pub(crate) use automation_journal::SummaryProvider as AutomationSummaryProvider;
pub(crate) use automation_journal::{Draft as SwarmJournalDraft, Event as SwarmJournalEdit};
pub(crate) use automation_journal::{
    Estimate as AutomationEstimate, ModelEstimate as AutomationModelEstimate,
};
mod connection;
pub(crate) mod firing;
mod form;
pub(crate) mod imported_archive;
pub(crate) mod imported_privileged;
pub(crate) mod imported_transfer;
mod journal_retention_java;
mod journal_search;
pub(crate) mod journal_stats;
mod notifications;
pub(crate) mod ownership;
pub(crate) mod privileged;
mod remote;
pub(crate) mod rsync;
pub(crate) mod rsync_authority;
mod run_detail;
pub(crate) mod script;
pub(crate) mod sync;
mod view;
pub(crate) mod webhooks;
mod working_directory;

#[cfg(test)]
mod tests;

pub(crate) use connection::TargetGroups;
pub(crate) use connection::{ConnectionChoice, ConnectionRef, MAX_SELECTED_GROUPS, MAX_TARGETS};
use firing::RunProposalView;
use form::{Draft, Field, Mode, Source};
use journal_search::{JournalColumn, JournalSearchMode};
pub(crate) use remote::{
    Archive, ArchiveExclusions, ArchiveFormat, MAX_ARCHIVE_EXCLUDE_TEXT, MAX_ARCHIVE_EXCLUDES,
    MAX_ARCHIVE_SOURCES, RemoteAction, RemotePath, SymbolicMode, mode_from_text,
    owner_id_from_text, windows_drive_ancestors,
};
use script::{Language, MAX_COMMAND_BYTES, shell_quote};
pub(crate) use script::{Script, SnippetLink};
use view::{DiagramView, JournalSort, JournalSortColumn, Layout};
use working_directory::{Browser as DirectoryBrowser, Message as DirectoryMessage};

const SAVE_RETRY: Duration = Duration::from_secs(2);
const MAX_WORKING_DIRECTORY_BYTES: usize = 4096;
pub(crate) const DEFAULT_JOURNAL_RETENTION_DAYS: u16 = 14;
pub(crate) const MAX_JOURNAL_RETENTION_DAYS: u16 = 3650;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SudoMode {
    #[default]
    Disabled,
    Passwordless,
    Vault,
}

impl SudoMode {
    pub(super) const ALL: [Self; 3] = [Self::Disabled, Self::Passwordless, Self::Vault];

    fn is_disabled(&self) -> bool {
        *self == Self::Disabled
    }
}

impl fmt::Display for SudoMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Disabled => "Run as the SSH user",
            Self::Passwordless => "Passwordless sudo",
            Self::Vault => "sudo with a vault password",
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum JournalDetail {
    #[default]
    Off,
    LimitedRedacted,
    Full,
}

impl JournalDetail {
    pub(super) const ALL: [Self; 3] = [Self::LimitedRedacted, Self::Full, Self::Off];

    fn is_off(&self) -> bool {
        *self == Self::Off
    }
}

impl fmt::Display for JournalDetail {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Off => "Off",
            Self::LimitedRedacted => "Limited, redacted",
            Self::Full => "Full, redacted",
        })
    }
}

/// Persisted metadata for one unattended SSH action.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SshCommand {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub host: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub port: u16,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub username: String,
    #[serde(default, skip_serializing_if = "path_is_empty")]
    pub key_file: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) connection: Option<ConnectionRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) additional_connections: Vec<ConnectionRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) target_groups: Option<TargetGroups>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) working_directory: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) script: Option<Script>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) snippet: Option<SnippetLink>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) remote: Option<RemoteAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) agent: Option<agent::Action>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) imported_ai: Option<ImportedAiAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) imported_transfer: Option<Box<imported_transfer::Draft>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) imported_privileged: Option<Box<imported_privileged::Draft>>,
    #[serde(default, skip_serializing_if = "SudoMode::is_disabled")]
    pub(crate) sudo: SudoMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub virtual_terminal: Option<virtual_terminal::Options>,
    #[serde(default, skip_serializing_if = "JournalDetail::is_off")]
    pub(crate) journal_detail: JournalDetail,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) automation_journal: Option<crate::session_journal::automation::Options>,
    #[serde(default, skip_serializing_if = "notifications::Settings::is_default")]
    pub(crate) desktop_notices: notifications::Settings,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) webhook_targets: Vec<webhooks::Target>,
}

pub(crate) const IMPORTED_AI_REVIEW_REQUIRED: &str =
    "Review the imported AI action and select a local profile before running or enabling this job.";

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportedAiAction {
    pub(crate) kind: agent::Kind,
    pub(crate) goal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) profile_source: Option<crate::ai_manager::JavaProfileSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) working_directory: Option<String>,
    pub(crate) parallelism: u8,
    pub(crate) swarm_read_only: bool,
}

impl ImportedAiAction {
    pub(crate) fn probe(&self) -> agent::Probe {
        match self.kind {
            agent::Kind::Agent => agent::Probe::PerFiring,
            agent::Kind::Swarm if self.swarm_read_only => agent::Probe::SwarmReadOnly,
            agent::Kind::Swarm => agent::Probe::SwarmIterative,
        }
    }

    fn validate(&self) -> Result<(), &'static str> {
        if self.goal.trim().is_empty()
            || self.goal.len() > MAX_COMMAND_BYTES
            || !self
                .goal
                .chars()
                .all(crate::session_journal::is_display_character)
            || self
                .profile_id
                .as_deref()
                .is_some_and(|id| crate::ai_manager::JavaProfileSource::parse_id(id) != Some(id))
            || self
                .profile_source
                .as_ref()
                .is_some_and(|source| self.profile_id.as_deref() != Some(source.id()))
            || self.working_directory.as_deref().is_some_and(|path| {
                path.trim().is_empty()
                    || path.len() > MAX_WORKING_DIRECTORY_BYTES
                    || path.chars().any(char::is_control)
                    || !path
                        .chars()
                        .all(crate::session_journal::is_display_character)
            })
            || !(1..=kortty_jobs::MAX_SWARM_PARALLELISM).contains(&usize::from(self.parallelism))
            || self.kind == agent::Kind::Agent && (self.parallelism != 1 || !self.swarm_read_only)
        {
            return Err("Invalid imported AI action settings");
        }
        Ok(())
    }
}

impl fmt::Debug for SshCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("SshCommand").finish_non_exhaustive()
    }
}

impl SshCommand {
    pub(crate) fn validated(mut self) -> Result<Self, String> {
        self.validate_target_selection()?;
        self.validate_elevation()?;
        webhooks::validate(&self.webhook_targets)?;
        if self.connection.is_some() {
            if !self.host.is_empty()
                || self.port != 0
                || !self.username.is_empty()
                || !path_is_empty(&self.key_file)
            {
                return Err("Choose either a saved SSH connection or manual SSH details".into());
            }
        } else {
            self.validate_manual_connection()?;
        }
        match self.source()? {
            ActionSource::Command(command) => {
                self.with_working_directory(Cow::Borrowed(command))?;
            }
            ActionSource::Script(script) => {
                self.with_working_directory(Cow::Owned(script.command()?))?;
            }
            ActionSource::Snippet(link) => {
                self.working_directory()?;
                link.validate()?;
            }
            ActionSource::Remote(action) => {
                action.validate_definition()?;
                if let RemoteAction::ImportedArchive(archive) = action
                    && (self.connection.as_ref() != Some(&archive.route)
                        || !self.additional_connections.is_empty()
                        || self.target_groups.is_some())
                {
                    return Err("Keep the exact saved SSH route for the imported archive.".into());
                }
                if matches!(
                    action,
                    RemoteAction::Sync(_)
                        | RemoteAction::ServerArchive(_)
                        | RemoteAction::PrivilegedArchive(_)
                        | RemoteAction::PrivilegedTreeUpload(_)
                        | RemoteAction::PrivilegedTreeDownload(_)
                ) && (self.connection.is_none() || !self.additional_connections.is_empty())
                {
                    return Err(
                        "Reviewed copy and server-only archive actions require one saved SSH connection."
                            .into(),
                    );
                }
            }
            ActionSource::Agent(agent) => agent.validate(&self)?,
            ActionSource::ImportedAi(draft) => draft.validate()?,
            ActionSource::ImportedTransfer(draft) => {
                draft.validate()?;
                if let imported_transfer::Draft::StagedSftp(staged) = draft {
                    staged.require_route(
                        self.connection.as_ref(),
                        &self.additional_connections,
                        self.target_groups.is_some(),
                    )?;
                }
            }
            ActionSource::ImportedPrivileged(draft) => {
                draft.validate(self.targets(), self.target_groups.is_some())?;
            }
        }
        if let Some(options) = self.virtual_terminal {
            options.validate()?;
        }
        if !self.journal_detail.is_off() && !self.supports_journal_output() {
            return Err("Separate journal output requires a command, script, or snippet.".into());
        }
        if let Some(options) = &self.automation_journal {
            options.validate().map_err(|error| error.to_string())?;
            if options.enabled && !self.supports_automation_journal() {
                return Err(
                    "Per-target journals require a command, script, snippet, file action, or supported Agent or Swarm mode."
                        .into(),
                );
            }
        }
        Ok(self)
    }

    pub(crate) fn require_executable(&self) -> Result<(), &'static str> {
        self.require_resolved_action()?;
        if let Some((transfer, upload)) =
            self.remote.as_ref().and_then(RemoteAction::rsync_transfer)
            && (!self.additional_connections.is_empty() || !transfer.routes.is_empty())
        {
            self.validate_target_selection()?;
            transfer.require_group_review(upload)?;
        }
        Ok(())
    }

    fn require_resolved_action(&self) -> Result<(), &'static str> {
        if self.imported_ai.is_some() {
            return Err(IMPORTED_AI_REVIEW_REQUIRED);
        }
        if let Some(draft) = self.imported_transfer.as_ref() {
            return Err(draft.review_required());
        }
        if self.imported_privileged.is_some() {
            return Err(imported_privileged::REVIEW_REQUIRED);
        }
        if let Some(action) = &self.remote {
            action.require_executable()?;
        }
        Ok(())
    }

    pub(crate) fn validate_definition(job: &kortty_jobs::Job<Self>) -> Result<(), String> {
        if job.enabled() {
            job.action().require_resolved_action()?;
        }
        job.action().clone().validated().map(drop)
    }

    pub(crate) fn supports_automation_journal(&self) -> bool {
        self.agent
            .as_ref()
            .is_none_or(|action| action.probe.supports_target_journal(action.kind))
    }

    fn supports_journal_output(&self) -> bool {
        self.remote.is_none()
            && self.agent.is_none()
            && self.imported_ai.is_none()
            && self.imported_transfer.is_none()
            && self
                .imported_privileged
                .as_deref()
                .is_none_or(|draft| draft.settings.shell_options().is_some())
    }

    pub(crate) fn validate_elevation(&self) -> Result<(), &'static str> {
        if let Some((transfer, upload)) =
            self.remote.as_ref().and_then(RemoteAction::rsync_transfer)
            && transfer.uses_passwordless_sudo()
        {
            if self.sudo != SudoMode::Disabled
                || self.connection.is_none()
                || self.virtual_terminal.is_some()
                || self.agent.is_some()
            {
                return Err(
                    "Passwordless rsync requires saved SSH targets and its dedicated Linux helper.",
                );
            }
            return transfer.validate(upload);
        }
        if let Some(
            action @ (RemoteAction::Privileged(_)
            | RemoteAction::PrivilegedFileAction(_)
            | RemoteAction::PrivilegedArchive(_)
            | RemoteAction::PrivilegedTreeUpload(_)
            | RemoteAction::PrivilegedTreeDownload(_)),
        ) = &self.remote
        {
            action.validate()?;
            if self.sudo == SudoMode::Disabled
                || self.connection.is_none()
                || !self.additional_connections.is_empty()
                || self.virtual_terminal.is_some()
                || self.agent.is_some()
            {
                return Err(
                    "Privileged file actions require sudo, one saved SSH connection, and no virtual terminal.",
                );
            }
            return Ok(());
        }
        if self.sudo != SudoMode::Disabled && (self.remote.is_some() || self.agent.is_some()) {
            return Err(
                "Choose a command, script, snippet, or explicitly reviewed privileged file action for sudo.",
            );
        }
        if self.sudo == SudoMode::Vault {
            if self.connection.is_none() {
                return Err("Choose a saved SSH connection to use a vault sudo password.");
            }
            if self.virtual_terminal.is_some() {
                return Err("Jobs with a sudo password cannot use a virtual terminal.");
            }
        }
        Ok(())
    }

    pub(crate) fn targets(&self) -> impl Iterator<Item = &ConnectionRef> {
        self.connection
            .iter()
            .chain(self.additional_connections.iter())
    }

    fn uses_swarm_targets(&self) -> bool {
        self.agent
            .as_ref()
            .is_some_and(|action| action.kind == agent::Kind::Swarm)
            || self
                .imported_ai
                .as_ref()
                .is_some_and(|draft| draft.kind == agent::Kind::Swarm)
    }

    pub(crate) fn validate_target_selection(&self) -> Result<(), &'static str> {
        if !self.additional_connections.is_empty() && self.connection.is_none() {
            return Err("Choose a primary saved SSH connection before adding targets");
        }
        let swarm = self.uses_swarm_targets();
        if let Some(groups) = &self.target_groups {
            groups.validate(swarm)?;
            if self.connection.is_none()
                || !groups
                    .explicit
                    .iter()
                    .zip(self.targets())
                    .all(|(source, target)| source == target)
                || groups.explicit.len() > self.targets().count()
            {
                return Err("Refresh the saved connection group targets before continuing.");
            }
        }
        if self.additional_connections.len() >= connection::target_limit(swarm) {
            return Err(connection::target_limit_notice(swarm));
        }
        let mut selected = BTreeSet::new();
        for target in self.targets() {
            target.validate()?;
            if !selected.insert(target.id) {
                return Err("Choose each saved SSH connection only once");
            }
        }
        let rsync = self.remote.as_ref().and_then(RemoteAction::rsync_transfer);
        if let Some((transfer, upload)) = rsync {
            let primary = self
                .connection
                .as_ref()
                .ok_or("Reviewed rsync requires saved SSH connections.")?;
            transfer.validate_routes(primary, &self.additional_connections, upload)?;
        }
        if !self.additional_connections.is_empty() {
            if self.virtual_terminal.is_some() {
                return Err("Virtual terminal recordings require one SSH connection");
            }
            if rsync.is_none()
                && self
                    .remote
                    .as_ref()
                    .is_some_and(RemoteAction::writes_local_file)
            {
                return Err("A local download destination requires one SSH connection");
            }
        }
        Ok(())
    }

    pub(crate) fn require_current_target_groups(
        &self,
        store: &ConnectionStore,
    ) -> Result<(), &'static str> {
        if let Some(groups) = &self.target_groups {
            let resolved = groups.resolve(store, self.uses_swarm_targets())?;
            if !resolved.iter().eq(self.targets()) {
                return Err(connection::GROUP_TARGETS_CHANGED);
            }
        }
        Ok(())
    }

    pub(crate) fn require_installed_target_groups(&self) -> Result<(), &'static str> {
        if self.target_groups.is_none() {
            return Ok(());
        }
        let store = crate::profile::connections_path()
            .and_then(ConnectionStore::open)
            .map_err(|_| "The saved connection groups are unavailable.")?;
        self.require_current_target_groups(&store)
    }

    fn validate_manual_connection(&mut self) -> Result<(), String> {
        let endpoint = Endpoint::new(&self.host, self.port).map_err(|error| error.to_string())?;
        self.host = endpoint.wire_host().into_owned();
        if self.username.trim().is_empty()
            || self.username.len() > 255
            || self.username.chars().any(char::is_control)
        {
            return Err("Enter a valid SSH username".into());
        }
        if !self.key_file.is_absolute()
            || self.key_file.as_os_str().len() > 4096
            || self
                .key_file
                .to_str()
                .is_none_or(|path| path.chars().any(char::is_control))
        {
            return Err("Enter an absolute OpenSSH private-key path".into());
        }
        Ok(())
    }

    pub(crate) fn command_for_exec(
        &self,
        snippets_path: Option<&Path>,
    ) -> Result<Cow<'_, str>, &'static str> {
        let command = match self.source()? {
            ActionSource::Command(command) => Cow::Borrowed(command),
            ActionSource::Script(script) => Cow::Owned(script.command()?),
            ActionSource::Snippet(link) => Cow::Owned(
                link.command(snippets_path.ok_or("The snippet library is unavailable")?)?,
            ),
            ActionSource::Remote(_) => return Err("SFTP actions require the SSH file subsystem"),
            ActionSource::Agent(_) => {
                return Err("Diagnostics require the reviewed AI job executor");
            }
            ActionSource::ImportedAi(_) => return Err(IMPORTED_AI_REVIEW_REQUIRED),
            ActionSource::ImportedTransfer(draft) => return Err(draft.review_required()),
            ActionSource::ImportedPrivileged(_) => {
                return Err(imported_privileged::REVIEW_REQUIRED);
            }
        };
        self.with_working_directory(command)
    }

    fn working_directory(&self) -> Result<Option<&str>, &'static str> {
        let path = self.working_directory.as_deref();
        if path.is_some_and(|path| {
            path.trim().is_empty()
                || path.len() > MAX_WORKING_DIRECTORY_BYTES
                || path.chars().any(char::is_control)
        }) {
            return Err("Enter a working directory of at most 4 KiB without control characters");
        }
        Ok(path)
    }

    fn with_working_directory<'a>(
        &self,
        command: Cow<'a, str>,
    ) -> Result<Cow<'a, str>, &'static str> {
        let Some(path) = self.working_directory()? else {
            return Ok(command);
        };
        // Disable CDPATH lookup and avoid interpreting a directory named "-" as OLDPWD.
        let path = if path.starts_with('-') {
            Cow::Owned(format!("./{path}"))
        } else {
            Cow::Borrowed(path)
        };
        let mut wrapped = String::from("CDPATH= cd ");
        shell_quote(&mut wrapped, &path);
        // A failing directory change must stop the whole command, including multiline scripts.
        wrapped.push_str(" || exit\n");
        wrapped.push_str(&command);
        if wrapped.len() > MAX_COMMAND_BYTES {
            return Err("The action and working directory exceed the 16 KiB SSH command limit");
        }
        Ok(Cow::Owned(wrapped))
    }

    pub(crate) fn remote_action(&self) -> Result<&RemoteAction, &'static str> {
        match self.source()? {
            ActionSource::Remote(action) => {
                action.validate()?;
                Ok(action)
            }
            _ => Err("This job does not contain an SFTP action"),
        }
    }

    fn source(&self) -> Result<ActionSource<'_>, &'static str> {
        if self.imported_ai.is_some()
            || self.imported_transfer.is_some()
            || self.imported_privileged.is_some()
        {
            if self.connection.is_none()
                || !self.command.is_empty()
                || self.working_directory.is_some()
                || self.script.is_some()
                || self.snippet.is_some()
                || self.remote.is_some()
                || self.agent.is_some()
                || self.virtual_terminal.is_some()
                || self.sudo != SudoMode::Disabled
                || !self.webhook_targets.is_empty()
            {
                return Err(
                    "An imported action draft requires saved SSH targets and no executable action",
                );
            }
            return match (
                &self.imported_ai,
                &self.imported_transfer,
                &self.imported_privileged,
            ) {
                (Some(draft), None, None) => Ok(ActionSource::ImportedAi(draft)),
                (None, Some(draft), None) => Ok(ActionSource::ImportedTransfer(draft)),
                (None, None, Some(draft)) => Ok(ActionSource::ImportedPrivileged(draft)),
                _ => Err("Choose one action for the SSH job"),
            };
        }
        match (
            self.command.as_str(),
            self.script.as_ref(),
            self.snippet.as_ref(),
            self.remote.as_ref(),
            self.agent.as_ref(),
        ) {
            ("", Some(script), None, None, None) => Ok(ActionSource::Script(script)),
            ("", None, Some(link), None, None) => Ok(ActionSource::Snippet(link)),
            ("", None, None, Some(_), None) if self.virtual_terminal.is_some() => {
                Err("SFTP actions cannot use a virtual terminal")
            }
            ("", None, None, Some(_), None) if self.working_directory.is_some() => {
                Err("SFTP actions use absolute paths instead of a working directory")
            }
            ("", None, None, Some(action), None) => Ok(ActionSource::Remote(action)),
            ("", None, None, None, Some(_)) if self.virtual_terminal.is_some() => {
                Err("Scheduled diagnostics run without a virtual terminal")
            }
            ("", None, None, None, Some(action)) => Ok(ActionSource::Agent(action)),
            (command, None, None, None, None)
                if !command.trim().is_empty()
                    && command.len() <= MAX_COMMAND_BYTES
                    && !command.contains('\0') =>
            {
                Ok(ActionSource::Command(command))
            }
            (_, None, None, None, None) => Err("Enter a nonempty SSH command of at most 16 KiB"),
            _ => Err("Choose one action for the SSH job"),
        }
    }
}

fn is_zero(value: &u16) -> bool {
    *value == 0
}

fn path_is_empty(path: &Path) -> bool {
    path.as_os_str().is_empty()
}

enum ActionSource<'a> {
    Command(&'a str),
    Script(&'a Script),
    Snippet(&'a SnippetLink),
    Remote(&'a RemoteAction),
    Agent(&'a agent::Action),
    ImportedAi(&'a ImportedAiAction),
    ImportedTransfer(&'a imported_transfer::Draft),
    ImportedPrivileged(&'a imported_privileged::Draft),
}

#[derive(Clone, Eq, PartialEq)]
pub struct SnippetChoice {
    id: SnippetId,
    label: String,
}

impl fmt::Display for SnippetChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.label)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Page {
    #[default]
    Job,
    Action,
    Journal,
    Diagram,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DirectoryField {
    WorkingDirectory,
    RemotePath,
    RemoteDestination,
}

impl DirectoryField {
    fn field(self) -> Field {
        match self {
            Self::WorkingDirectory => Field::WorkingDirectory,
            Self::RemotePath => Field::RemotePath,
            Self::RemoteDestination => Field::RemoteDestination,
        }
    }

    fn page(self) -> Page {
        match self {
            Self::WorkingDirectory => Page::Job,
            Self::RemotePath | Self::RemoteDestination => Page::Action,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Scope {
    #[default]
    All,
    Connected,
    Selected,
}

/// UI and runtime messages for the scheduler. Command and detail text is redacted from Debug.
#[derive(Clone)]
pub enum Message {
    Close,
    CloseConfirmed,
    HostedClose(u64, bool),
    Tick,
    Finished(Completion),
    FinishedWithSessionJournals(Completion, Vec<String>),
    Page(Page),
    ResizeJobs(pane_grid::ResizeEvent),
    ResizeJournal(pane_grid::ResizeEvent),
    JournalSearch(String),
    JournalSearchMode(JournalSearchMode),
    JournalColumns,
    JournalColumn(JournalColumn, bool),
    JournalSort(JournalSortColumn),
    EditJournalRetention(String),
    SaveJournalRetention,
    JournalRetentionJava(journal_retention_java::Action),
    JournalStatsReady(journal_stats::Request, journal_stats::Result),
    MenuStatusVisible(bool),
    New,
    Select(JobId),
    Edit(Field, String),
    AdoptImportedArchive,
    AdoptImportedTransfer(imported_transfer::Mode),
    ImportedStagingSudo(SudoMode),
    ImportedPrivilegedSudo(SudoMode),
    AdoptImportedPrivileged,
    ConfigureImportedRsync {
        directory: bool,
    },
    ConfigureImportedExistingRsync,
    ConfigureImportedExistingRsyncFile,
    ConfigureImportedMixedRsync,
    ConfigureImportedPasswordlessRsync {
        directory: bool,
        existing: bool,
    },
    ConfigureImportedPasswordlessMixedRsync,
    BrowseDirectory(DirectoryField),
    DirectoryBrowser(DirectoryMessage),
    Ownership(ownership::Message),
    EditCommand(text_editor::Action),
    EditAgentGoal(text_editor::Action),
    EditScriptArguments(text_editor::Action),
    EditArchiveSources(text_editor::Action),
    EditArchiveExcludes(text_editor::Action),
    EditSyncExcludes(text_editor::Action),
    EditRsyncSources(text_editor::Action),
    RsyncDestination(u64, String),
    RsyncExisting(bool),
    RsyncAllowVacantDirectory(bool),
    RsyncAllowVacantFile(bool),
    RsyncAllowVacantServerFile(bool),
    RsyncAllowVacantServerTree(bool),
    RsyncMixed(bool),
    RsyncIncludeSourceName(bool),
    RsyncMirror(bool),
    RsyncPasswordless(bool),
    RsyncStanding(bool),
    SyncStanding(bool),
    SyncReplaceChanged(bool),
    ReviewSync,
    SyncPreviewed(String, Result<Arc<sync::Review>, String>),
    ApproveSync,
    PrivilegedReplaceExisting(bool),
    PrivilegedStanding(bool),
    ReviewPrivileged,
    PrivilegedReplacementPreviewed(privileged::ReplacementPreviewed),
    ApprovePrivileged,
    DiscardPrivileged,
    DiscardSync,
    ReviewRsyncProgram,
    RsyncProgramReviewed(String, Result<Box<rsync::Reviewed>, String>),
    ApproveRsyncProgram,
    DiscardRsyncProgram,
    AgentProfile(agent::ProfileChoice),
    AgentProbe(agent::Probe),
    ReviewAgent,
    RequestAgentProposal(String),
    ReviewProposalSkills(String),
    ClassifyProposalSkills(String),
    CancelProposalSkills(String),
    ProposalSkillsFinished(String, Result<Vec<u64>, String>),
    ProposalQuotaFinished(String, u64, String),
    ProposalSkillQuotaFinished(String, u64, String),
    ProposalKnowledge(
        String,
        crate::workspace::terminal_agent::knowledge_review::Message,
    ),
    ProposalWeb(String, crate::workspace::terminal_ai::web::Message),
    RequireFreshWeb(String, bool),
    EditUnattendedWebTopic(String, String),
    EditUnattendedWebFollowups(String, bool),
    EditUnattendedWebEnvironment(String, bool),
    EditUnattendedWebRounds(String, bool),
    EditUnattendedWebRoundPages(String, bool),
    ReviewUnattendedWeb(String, String),
    RemoveUnattendedWeb(String),
    ReviewSwarmComparison(String),
    ApproveSwarmComparison(String),
    CancelSwarmComparison(String),
    SwarmComparisonQuotaFinished(String, String),
    AgentProposalProgress(String, agent::proposal::Progress),
    AgentProposed(String, Result<Arc<Vec<agent::proposal::Item>>, String>),
    ApproveAgent,
    ApproveProposalCommands(String),
    ApproveFiringRequest(String),
    DiscardAgentReview,
    ArchiveFormat(ArchiveFormat),
    ArchiveServerOnly(bool),
    ArchiveServerPath(bool),
    ArchiveReuseOutput(bool),
    ArchivePassword(CredentialChoice),
    OpenCredentialVault,
    Sudo(SudoMode),
    CommandSource(Source),
    ConnectionSelected(ConnectionChoice),
    ConnectionAdded(ConnectionChoice),
    ConnectionRemoved(u64),
    ConnectionGroupAdded(String),
    ConnectionGroupRemoved(String),
    RefreshConnectionGroups,
    ScriptLanguage(Language),
    SnippetSelected(SnippetChoice),
    Enabled(bool),
    ScheduleEnabled(bool),
    RecursiveDelete(bool),
    PrivilegedCopyTree(bool),
    VirtualTerminal(bool),
    JournalDetail(JournalDetail),
    AutomationJournal(automation_journal::Event),
    EstimateAutomationJournal,
    AutomationEstimateReady(u64, AutomationEstimate),
    DesktopNotices(notifications::Settings),
    AddWebhook(webhooks::Choice),
    WebhookFormat(usize, webhooks::Format),
    RemoveWebhook(usize),
    CaptureChanges(bool),
    LimitIsSuccess(bool),
    Weekday(Weekday, bool),
    AllWeekdays(bool),
    FixedTimeInput(String),
    AddFixedTimes,
    SelectFixedTime(Time),
    RemoveFixedTime(Time),
    Dependency(JobId, bool),
    Mode(Mode),
    Save,
    Run(bool),
    CancelRun(RunId),
    SelectActiveRun(RunId),
    ApproveRunProposal(ProposalTicket),
    RejectRunProposal(ProposalTicket),
    Delete,
    SwitchChain,
    Confirm,
    Dismiss,
    SelectRecord(RunId),
    MarkRecord(RunId, bool),
    SelectRunDetail(RunId, text_editor::Action),
    OpenRunDetailMenu(RunId, iced::Point),
    DismissRunDetailMenu(RunId),
    CopyRunDetail(RunId),
    OpenSessionJournals(RunId),
    OpenSessionJournal(RunId, String),
    OpenRecording(RunId),
    OpenSwarmChat(RunId),
    DeleteRecord,
    DiagramScope(Scope),
    DiagramDirection,
    DiagramTheme,
    DiagramCopy,
    DiagramPath(String),
    DiagramExport(Format),
    DiagramExported(Result<PathBuf, String>),
    Zoom(f32),
}

impl fmt::Debug for Message {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Finished(completion) | Self::FinishedWithSessionJournals(completion, _) => {
                formatter
                    .debug_tuple("Finished")
                    .field(&completion.id)
                    .field(&completion.outcome)
                    .finish_non_exhaustive()
            }
            Self::Close => formatter.write_str("Close"),
            Self::CloseConfirmed => formatter.write_str("CloseConfirmed"),
            Self::Tick => formatter.write_str("Tick"),
            _ => formatter.write_str("Scheduler edit"),
        }
    }
}

struct Active {
    job: JobId,
    started_at: SystemTime,
    trigger: RunTrigger,
    cancellation: kortty_jobs::Cancellation,
    swarm_chat: Option<ActiveSwarmChat>,
}

struct ActiveSwarmChat {
    profile: ProfileRef,
    targets: Vec<u64>,
}

impl ActiveSwarmChat {
    fn matches(&self, report: &SwarmReport) -> bool {
        self.targets
            .iter()
            .copied()
            .eq(report.targets.iter().map(|target| target.target))
    }
}

pub(crate) struct MenuRun<'a> {
    pub(crate) id: RunId,
    pub(crate) name: &'a str,
    pub(crate) cancelling: bool,
}

pub(crate) struct MenuDeadline<'a> {
    pub(crate) name: &'a str,
    pub(crate) due: SystemTime,
    pub(crate) local_time: String,
}

pub(crate) struct MenuStatus<'a> {
    pub(crate) running: Vec<MenuRun<'a>>,
    pub(crate) upcoming: Vec<MenuDeadline<'a>>,
}

struct PendingRecord {
    record: RunRecord,
    detail: Option<String>,
    output: Option<CapturedOutput>,
    recording: Option<JournalRecording>,
    session_journals: Vec<String>,
    swarm_report: Option<Box<SwarmReport>>,
    swarm_chat_origin: Option<SwarmChatOrigin>,
}

struct FinishedRun {
    completion: Completion,
    swarm_chat: Option<ActiveSwarmChat>,
    session_journals: Vec<String>,
    existing_rsync: bool,
}

enum Confirmation {
    Close,
    Run(JobId, bool, Vec<Blocker>),
    SyncCascade(sync::CascadeReview, Vec<Blocker>),
    Delete(JobId, Vec<String>),
    SwitchChain(JobId, bool),
    DeleteRecord(Vec<RunId>),
}

/// One scheduler, store and job lifecycle shared by every window and tab.
pub struct SchedulerWindow {
    store: SchedulerStore,
    scheduler: Scheduler<SshCommand>,
    zone: TimeZone,
    execution_allowed: bool,
    recording_allowed: bool,
    journal_output_allowed: bool,
    swarm_chats_allowed: bool,
    recording_directory: PathBuf,
    snippet_path: PathBuf,
    sync_store: sync::Store,
    sync_epoch: Option<[u8; 16]>,
    sync_cascades: BTreeMap<RunId, (sync::CascadeReview, Vec<Blocker>)>,
    agent_cascade_root_blockers: BTreeMap<RunId, Vec<Blocker>>,
    proposal_store: agent::proposal::Store,
    rsync_runs: rsync_authority::Runs,
    rsync_standing: rsync::standing::Store,
    snippet_choices: Vec<SnippetChoice>,
    connection_choices: Vec<ConnectionChoice>,
    connection_group_choices: Vec<String>,
    connection_store_path: Option<PathBuf>,
    archive_passwords: Vec<CredentialChoice>,
    webhook_choices: Vec<webhooks::Choice>,
    agent_profiles: agent::ProfileChoices,
    draining: bool,
    active: BTreeMap<RunId, Active>,
    pub(crate) firing_reviews: firing::Reviews,
    pending_starts: Vec<RunRequest<SshCommand>>,
    pending_records: VecDeque<PendingRecord>,
    notices: notifications::State,
    webhooks: webhooks::State,
    webhook_results: BTreeMap<RunId, BTreeMap<String, kortty_jobs::WebhookOutcome>>,
    webhook_retry_at: Option<Instant>,
    retry_at: Option<SystemTime>,
    journal_retention_days: u16,
    journal_retention_error: Option<String>,
    pub(crate) journal_stats: journal_stats::Cache,
    graph_revision: u64,
    diagram_revision: u64,
    status: Option<String>,
}

#[derive(Default)]
pub(crate) struct SchedulerView {
    page: Page,
    layout: Layout,
    draft: Draft,
    baseline: Option<(kortty_jobs::JobDefinition, SshCommand)>,
    dirty: bool,
    selected_run: Option<RunId>,
    run_detail: Option<run_detail::Editor>,
    journal_details: Option<(RunId, PathBuf, JournalDetails)>,
    marked_records: BTreeSet<RunId>,
    journal_search: journal_search::Search,
    journal_sort: JournalSort,
    journal_retention_draft: Option<(u16, String)>,
    journal_retention_java: journal_retention_java::Editor,
    confirm: Option<Confirmation>,
    diagram: DiagramView,
    directory_browser: Option<DirectoryBrowser>,
    ownership_review: Option<ownership::Review>,
    sync_review: sync::DraftReview,
    privileged_review: privileged::DraftReview,
    rsync_review: rsync::Editor,
    agent_review: Option<agent::Review>,
    agent_approval: Option<agent::Approved>,
    automation_journal_denied: bool,
    status: Option<String>,
    graph_revision: u64,
    diagram_revision: u64,
    pub(crate) hosted_close: Option<u64>,
}

pub(crate) struct SwarmHandoff {
    pub(crate) name: String,
    pub(crate) goal: String,
    pub(crate) profile: crate::ai_manager::ProfileRef,
    pub(crate) targets: Vec<ConnectionRef>,
    pub(crate) read_only: bool,
    pub(crate) journal: Option<crate::session_journal::automation::Options>,
}

pub(crate) enum JournalDetails {
    Loading,
    Ready(Arc<RunJournals>),
    Failed(String),
}

impl SchedulerView {
    pub(crate) fn automation_summary_profile(
        &self,
    ) -> Option<&crate::session_journal::summary_queue::ProfileChoice> {
        self.draft.automation_journal.summary_profile()
    }

    pub(crate) fn unresolved_automation_summary_profile(&self) -> bool {
        self.draft.automation_journal.unresolved_summary_profile()
    }

    pub(crate) fn automation_summary_job(&self) -> Option<&JobId> {
        self.draft.id.as_ref()
    }

    pub(crate) fn selected_journal_run(&self) -> Option<RunId> {
        self.selected_run.filter(|_| self.page == Page::Journal)
    }

    pub(crate) fn dismiss_run_detail_menu(&mut self, run: RunId) {
        if let Some(editor) = self.run_detail.as_mut().filter(|editor| editor.run == run) {
            editor.menu = None;
        }
    }

    pub(crate) fn show_journal_details(
        &mut self,
        runtime: &SchedulerWindow,
        run: RunId,
        root: PathBuf,
        details: JournalDetails,
    ) {
        if self.selected_journal_run() == Some(run) {
            self.journal_details = Some((run, root, details));
            runtime.sync_run_detail(self);
        }
    }

    pub(crate) fn retain_run_detail_journal_root(&mut self, root: Option<&Path>) {
        if self
            .journal_details
            .as_ref()
            .is_some_and(|(_, directory, _)| Some(directory.as_path()) != root)
        {
            self.journal_details = None;
            self.run_detail = None;
        }
    }

    pub(crate) fn journal_details(&self, run: RunId, root: &Path) -> Option<&JournalDetails> {
        self.journal_details
            .as_ref()
            .and_then(|(id, directory, details)| {
                (*id == run && directory == root).then_some(details)
            })
    }

    fn load_draft(&mut self, job: Option<&kortty_jobs::Job<SshCommand>>) {
        self.directory_browser = None;
        self.ownership_review = None;
        self.sync_review = sync::DraftReview::default();
        self.privileged_review = privileged::DraftReview::default();
        self.rsync_review = rsync::Editor::default();
        self.agent_review = None;
        self.agent_approval = None;
        let mut draft = job.map(Draft::from_job).unwrap_or_default();
        std::mem::swap(
            &mut draft.automation_journal.estimate,
            &mut self.draft.automation_journal.estimate,
        );
        self.draft = draft;
        self.baseline = job.map(|job| (job.definition(), job.action().clone()));
        self.dirty = false;
    }

    pub(crate) fn dirty(&self) -> bool {
        self.dirty
    }

    pub(crate) fn can_stage_swarm_handoff(&self) -> bool {
        !self.dirty && self.confirm.is_none() && self.hosted_close.is_none()
    }

    pub(crate) fn editing_regular_transfer(&self) -> bool {
        self.draft.regular_transfer()
    }

    pub(crate) fn editing_privileged_file_action(&self) -> bool {
        self.draft.privileged_file_action() || self.draft.privileged_archive()
    }

    pub(crate) fn editing_imported_privileged(&self) -> bool {
        self.draft.has_imported_privileged()
    }

    pub(crate) fn editing_privileged_tree(&self) -> bool {
        self.draft.privileged_tree()
    }

    pub(crate) fn finish_automation_estimate(&mut self, ticket: u64, estimate: AutomationEstimate) {
        self.draft
            .automation_journal
            .estimate
            .finish(ticket, estimate);
    }

    pub(crate) fn clear_automation_estimate(&mut self) {
        self.draft.automation_journal.estimate.clear();
    }

    pub(crate) fn set_automation_journal_allowed(&mut self, allowed: bool) {
        self.automation_journal_denied = !allowed;
        if !allowed {
            self.clear_automation_estimate();
        }
    }

    pub(crate) fn report_status(&mut self, status: String) {
        self.status = Some(status);
    }

    fn edited_journal_retention_days(&self) -> Option<u16> {
        let (_, input) = self.journal_retention_draft.as_ref()?;
        if input.len() > 4 || !input.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let days = input.parse::<u16>().ok()?;
        (days <= MAX_JOURNAL_RETENTION_DAYS).then_some(days)
    }

    pub(crate) fn journal_retention_change(&self, runtime: &SchedulerWindow) -> Option<u16> {
        let (current, _) = self.journal_retention_draft.as_ref()?;
        let days = self.edited_journal_retention_days()?;
        (self.page == Page::Journal
            && self.confirm.is_none()
            && !runtime.draining
            && runtime.pending_records.is_empty()
            && *current == runtime.journal_retention_days
            && days != *current)
            .then_some(days)
    }

    pub(crate) fn update_journal_retention_java(
        &mut self,
        runtime: &SchedulerWindow,
        action: journal_retention_java::Action,
    ) -> Task<Message> {
        if self.page != Page::Journal || self.confirm.is_some() || runtime.draining {
            return Task::none();
        }
        let saved = runtime.journal_retention_days;
        let (reviewed, task) = self.journal_retention_java.update(saved, action);
        if let Some(days) = reviewed {
            self.journal_retention_draft = Some((saved, days.to_string()));
        }
        task.map(Message::JournalRetentionJava)
    }

    pub(crate) fn request_close(&mut self) -> bool {
        self.directory_browser = None;
        self.ownership_review = None;
        self.journal_retention_java.close();
        self.sync_review = sync::DraftReview::default();
        self.privileged_review = privileged::DraftReview::default();
        self.rsync_review = rsync::Editor::default();
        self.agent_review = None;
        self.agent_approval = None;
        self.confirm = self.dirty.then_some(Confirmation::Close);
        !self.dirty
    }

    pub(crate) fn close_pending(&self) -> bool {
        matches!(self.confirm, Some(Confirmation::Close))
    }

    pub(crate) fn cancel_close(&mut self) {
        if self.close_pending() {
            self.confirm = None;
        }
    }

    pub(crate) fn directory_browser_target(
        &self,
        field: DirectoryField,
    ) -> Option<(&ConnectionRef, &str)> {
        if self.page != field.page() || self.directory_browser.is_some() || self.confirm.is_some() {
            return None;
        }
        self.draft.directory_browser_target(field)
    }

    pub(crate) fn open_directory_browser(
        &mut self,
        field: DirectoryField,
        target: String,
        path: String,
        credentials: Credentials,
    ) -> Task<Message> {
        let (browser, task) = DirectoryBrowser::open(field, target, path, credentials);
        self.directory_browser = Some(browser);
        task
    }

    pub(crate) fn browsing_directory(&self) -> bool {
        self.directory_browser.is_some()
    }

    pub(crate) fn update_directory_browser(&mut self, message: DirectoryMessage) -> Task<Message> {
        let Some(browser) = self.directory_browser.as_mut() else {
            return Task::none();
        };
        match message {
            DirectoryMessage::Cancel => self.directory_browser = None,
            DirectoryMessage::Choose => {
                if let Some(path) = browser.selected_path() {
                    self.draft.set(browser.field.field(), path.to_owned());
                    self.dirty = true;
                    self.directory_browser = None;
                }
            }
            message => return browser.update(message),
        }
        Task::none()
    }
}

impl SchedulerWindow {
    /// Opens an exclusive, native scheduler profile. Execution starts disabled.
    #[cfg(test)]
    pub fn open(path: PathBuf) -> Result<Self, String> {
        Self::open_with_journal_retention(path, 0)
    }

    pub(crate) fn open_with_journal_retention(
        path: PathBuf,
        journal_retention_days: u16,
    ) -> Result<Self, String> {
        let store = SchedulerStore::open(&path).map_err(|error| error.to_string())?;
        let recording_directory = path.with_file_name("job-recordings");
        let mut scheduler = store
            .restore_with_actions::<SshCommand>()
            .map_err(|error| error.to_string())?;
        for job in scheduler.graph().jobs() {
            SshCommand::validate_definition(job)
                .map_err(|error| format!("Job {} has invalid settings ({error})", job.id()))?;
        }
        let (zone, status) = match TimeZone::try_system() {
            Ok(zone) => (zone, None),
            Err(_) => (
                TimeZone::UTC,
                Some("System time zone unavailable. Scheduler uses UTC.".into()),
            ),
        };
        let now = SystemTime::now();
        scheduler
            .initialize_schedules(zone.clone(), now)
            .map_err(|error| error.to_string())?;
        virtual_terminal::remove_orphans(&recording_directory, store.journal_recordings());
        let profile = path
            .parent()
            .ok_or("The scheduler profile has no parent folder.")?;
        let mut sync_epoch = [0; 16];
        let sync_epoch = getrandom::fill(&mut sync_epoch).ok().map(|()| sync_epoch);
        let mut window = Self {
            store,
            scheduler,
            zone,
            execution_allowed: false,
            recording_allowed: false,
            journal_output_allowed: false,
            swarm_chats_allowed: false,
            recording_directory,
            snippet_path: path.with_file_name(snippets::FILE_NAME),
            sync_store: sync::Store::new(profile),
            sync_epoch,
            sync_cascades: BTreeMap::new(),
            agent_cascade_root_blockers: BTreeMap::new(),
            proposal_store: agent::proposal::Store::new(profile),
            rsync_runs: rsync_authority::Runs::default(),
            rsync_standing: rsync::standing::Store::new(profile.to_owned()),
            snippet_choices: Vec::new(),
            connection_choices: vec![ConnectionChoice::Manual],
            connection_group_choices: Vec::new(),
            connection_store_path: None,
            archive_passwords: Vec::new(),
            webhook_choices: Vec::new(),
            agent_profiles: agent::ProfileChoices::default(),
            draining: false,
            active: BTreeMap::new(),
            firing_reviews: firing::Reviews::default(),
            pending_starts: Vec::new(),
            pending_records: VecDeque::new(),
            notices: notifications::State::default(),
            webhooks: webhooks::State::default(),
            webhook_results: BTreeMap::new(),
            webhook_retry_at: None,
            retry_at: None,
            journal_retention_days,
            journal_retention_error: None,
            journal_stats: journal_stats::Cache::default(),
            graph_revision: 0,
            diagram_revision: 0,
            status,
        };
        window.expire_journal(now);
        window.refresh_snippets();
        Ok(window)
    }

    pub(crate) fn new_view(&self) -> SchedulerView {
        let mut view = SchedulerView {
            graph_revision: self.graph_revision,
            diagram_revision: self.diagram_revision,
            ..SchedulerView::default()
        };
        view.load_draft(self.scheduler.graph().jobs().first());
        view
    }

    pub(crate) fn stage_swarm_handoff(
        &self,
        view: &mut SchedulerView,
        handoff: SwarmHandoff,
    ) -> Result<(), String> {
        if !view.can_stage_swarm_handoff() || self.draining {
            return Err("The Scheduler editor is busy.".into());
        }
        if self.agent_profiles.selected(&handoff.profile).is_none() {
            return Err("The selected AI profile is unavailable in JobScheduler.".into());
        }
        if handoff.targets.iter().any(|target| {
            !self
                .connection_choices
                .iter()
                .any(|choice| choice.reference() == Some(target))
        }) {
            return Err(
                "A selected saved SSH connection changed. Refresh the Swarm targets.".into(),
            );
        }
        let draft = Draft::from_swarm_handoff(handoff)?;
        view.load_draft(None);
        view.draft = draft;
        view.page = Page::Job;
        view.dirty = true;
        view.status = Some("Swarm draft ready. Review the action and save to keep it.".into());
        Ok(())
    }

    pub(crate) fn sync_view(&self, view: &mut SchedulerView) {
        view.reconcile_journal_selection(self);
        if view
            .journal_retention_draft
            .as_ref()
            .is_some_and(|(days, _)| *days != self.journal_retention_days)
        {
            view.journal_retention_draft = None;
        }
        if view.graph_revision != self.graph_revision {
            view.graph_revision = self.graph_revision;
            view.ownership_review = None;
            let stale_sftp = view.sync_review.discard_standing();
            let stale_proposal = view
                .agent_review
                .as_ref()
                .and_then(|review| review.model.as_ref())
                .is_some_and(|model| {
                    model.skills.is_some() || model.has_reviewed_firing_guidance()
                });
            if stale_proposal {
                view.discard_proposal_review();
            }
            if !view.dirty
                && !self.draft_matches(view)
                && let Some(id) = &view.draft.id
            {
                let graph = self.scheduler.graph();
                view.load_draft(graph.job(id).or_else(|| graph.jobs().first()));
            }
            if !view.close_pending()
                && (view.confirm.take().is_some() || stale_proposal || stale_sftp)
            {
                view.status =
                    Some("Jobs changed. Review the current settings and try again.".into());
            }
        }
        if view.diagram_revision != self.diagram_revision {
            self.refresh_diagram(view);
        }
        self.sync_run_detail(view);
    }

    fn sync_run_detail(&self, view: &mut SchedulerView) {
        let editor = view.selected_journal_run().and_then(|run| {
            let record = self.store.journal_record(run)?;
            let linked = view
                .journal_details
                .as_ref()
                .filter(|(id, _, _)| *id == run)
                .map(|(_, root, details)| (root.as_path(), details));
            let sources = run_detail::Sources::for_run(self, record, linked)?;
            Some((run, sources))
        });
        match editor {
            Some((run, sources))
                if !view
                    .run_detail
                    .as_ref()
                    .is_some_and(|editor| editor.matches(run, &sources)) =>
            {
                view.run_detail = Some(run_detail::Editor::new(run, sources));
            }
            None => view.run_detail = None,
            _ => {}
        }
    }

    pub(crate) fn snippet_path(&self) -> &Path {
        &self.snippet_path
    }

    pub(crate) fn refresh_snippets(&mut self) {
        let mut choices = Library::open(self.snippet_path.clone()).map_or_else(
            |_| Vec::new(),
            |library| {
                library
                    .entries()
                    .iter()
                    .filter(|entry| Language::from_snippet(entry.language()).is_some())
                    .map(|entry| SnippetChoice {
                        id: entry.id(),
                        label: format!("{} ({})", entry.name(), entry.language()),
                    })
                    .collect()
            },
        );
        choices.sort_by_cached_key(|choice| choice.label.to_lowercase());
        self.snippet_choices = choices;
    }

    pub(crate) fn set_connections(&mut self, store: Option<&ConnectionStore>) {
        self.connection_choices.truncate(1);
        self.connection_group_choices.clear();
        self.connection_store_path = store.map(|store| store.path().to_owned());
        if let Some(store) = store {
            self.connection_group_choices
                .extend(store.groups().map(str::to_owned));
            self.connection_choices
                .extend(
                    store
                        .connections()
                        .iter()
                        .map(|profile| ConnectionChoice::Saved {
                            reference: profile.into(),
                            name: profile.name.clone(),
                        }),
                );
            self.connection_choices[1..]
                .sort_by_cached_key(|choice| choice.to_string().to_lowercase());
        }
    }

    fn current_connection_store(&self) -> Result<ConnectionStore, String> {
        let path = self
            .connection_store_path
            .clone()
            .or_else(|| crate::profile::connections_path().ok())
            .ok_or("The saved connection groups are unavailable.")?;
        ConnectionStore::open(path)
            .map_err(|_| "The saved connection groups are unavailable.".into())
    }

    fn build_current_draft(
        &self,
        view: &SchedulerView,
        id: JobId,
    ) -> Result<kortty_jobs::Job<SshCommand>, String> {
        self.require_current_draft_groups(view)?;
        view.draft.build(id)
    }

    pub(crate) fn require_current_draft_groups(&self, view: &SchedulerView) -> Result<(), String> {
        if view.draft.has_connection_groups() {
            view.draft
                .require_current_groups(&self.current_connection_store()?)?;
        }
        Ok(())
    }

    fn change_connection_groups(
        &mut self,
        view: &mut SchedulerView,
        change: impl FnOnce(&mut Draft, &ConnectionStore) -> (bool, Option<&'static str>),
    ) {
        let store = match self.current_connection_store() {
            Ok(store) => store,
            Err(error) => {
                view.status = Some(error);
                return;
            }
        };
        self.set_connections(Some(&store));
        let (changed, problem) = change(&mut view.draft, &store);
        if changed {
            view.dirty = true;
            view.sync_review = sync::DraftReview::default();
            view.privileged_review = privileged::DraftReview::default();
            view.rsync_review = rsync::Editor::default();
            view.agent_review = None;
            view.agent_approval = None;
            view.directory_browser = None;
        }
        view.status = Some(
            problem
                .unwrap_or(if changed {
                    "Connection targets updated. Review the action before saving."
                } else if view.draft.has_connection_groups() {
                    "The selected connection targets are unchanged."
                } else {
                    "Connection groups refreshed."
                })
                .into(),
        );
    }

    pub(crate) fn set_archive_passwords(&mut self, mut passwords: Vec<CredentialChoice>) {
        passwords.sort_by_cached_key(|choice| choice.label.to_lowercase());
        self.archive_passwords = passwords;
    }

    pub(crate) fn set_webhook_choices(&mut self, mut choices: Vec<webhooks::Choice>) {
        choices.sort_by(|a, b| a.label.cmp(&b.label).then_with(|| a.id.cmp(&b.id)));
        self.webhook_choices = choices;
    }

    pub(crate) fn set_agent_profiles(&mut self, config: Option<&Configuration>, allow_cli: bool) {
        self.agent_profiles = config.map_or_else(agent::ProfileChoices::default, |config| {
            agent::ProfileChoices::new(config, allow_cli)
        });
    }

    /// Applies the host application's execution policy and cancels work on revocation.
    pub fn set_execution_allowed(&mut self, allowed: bool) {
        if self.execution_allowed == allowed {
            return;
        }
        self.execution_allowed = allowed;
        if !allowed {
            self.cancel_runs();
        }
    }

    pub fn set_recording_allowed(&mut self, allowed: bool) {
        self.recording_allowed = allowed;
    }

    pub(crate) fn set_journal_output_allowed(&mut self, allowed: bool) {
        self.journal_output_allowed = allowed;
        if !allowed {
            for pending in &mut self.pending_records {
                pending.output = None;
            }
        }
    }

    pub(crate) fn set_swarm_chats_allowed(&mut self, allowed: bool) {
        self.swarm_chats_allowed = allowed;
    }

    pub(crate) fn profile_root(&self) -> &Path {
        self.store.profile_root()
    }

    pub(crate) fn committed_swarm_chat(
        &self,
        id: RunId,
    ) -> Option<(&RunRecord, &SwarmReport, &SwarmChatOrigin)> {
        Some((
            self.store.journal_record(id)?,
            self.store.journal_swarm_report(id)?,
            self.store.journal_swarm_chat_origin(id)?,
        ))
    }

    pub(crate) fn next_swarm_chat_seed(
        &self,
        ready: impl Fn(&SwarmChatSeed) -> bool,
    ) -> Option<SwarmChatSeed> {
        self.store.next_swarm_chat_seed(ready).cloned()
    }

    pub(crate) fn has_swarm_chat_seed(&self) -> bool {
        self.store.next_swarm_chat_seed(|_| true).is_some()
    }

    pub(crate) fn selected_swarm_chat(
        &self,
        view: &SchedulerView,
        id: RunId,
    ) -> Option<(&SwarmReport, &SwarmChatOrigin)> {
        if view.page != Page::Journal
            || view.selected_run != Some(id)
            || view.confirm.is_some()
            || view.hosted_close.is_some()
            || !view.journal_record_visible(self, id)
        {
            return None;
        }
        let (_, report, origin) = self.committed_swarm_chat(id)?;
        Some((report, origin))
    }

    pub(crate) fn acknowledge_swarm_chat_seed(&mut self, nonce: &str) -> Result<bool, StoreError> {
        self.store.acknowledge_swarm_chat_seed(nonce)
    }

    pub fn capture_directory(&self) -> Option<PathBuf> {
        self.recording_allowed
            .then(|| self.recording_directory.clone())
    }

    pub fn recording_path(&self, id: RunId) -> Option<PathBuf> {
        let name = self.store.journal_recording(id)?;
        Some(virtual_terminal::path_for(
            &self.recording_directory,
            id,
            name,
        ))
    }

    pub(crate) fn selected_recording_path(
        &self,
        view: &SchedulerView,
        id: RunId,
    ) -> Option<PathBuf> {
        self.selected_journal_record(view, id)?;
        self.recording_path(id)
    }

    pub(crate) fn selected_session_journals(
        &self,
        view: &SchedulerView,
        id: RunId,
    ) -> Option<(&JobId, &[String])> {
        let record = self.selected_journal_record(view, id)?;
        Some((&record.job, self.store.session_journals(id)?))
    }

    fn selected_journal_record(&self, view: &SchedulerView, id: RunId) -> Option<&RunRecord> {
        if view.page != Page::Journal
            || view.selected_run != Some(id)
            || !view.journal_record_visible(self, id)
        {
            return None;
        }
        self.store.journal_record(id)
    }

    /// Earliest scheduler or journal retry deadline, independent of window visibility.
    pub fn next_due(&self) -> Option<SystemTime> {
        self.retry_at
            .or_else(|| self.can_run().then(|| self.scheduler.next_due()).flatten())
    }

    /// True while remote work has been dispatched or is waiting for its durable predecessor record.
    pub fn has_active_runs(&self) -> bool {
        !self.active.is_empty() || !self.pending_starts.is_empty()
    }

    pub(crate) fn menu_status(&self, now: SystemTime, limit: usize) -> Option<MenuStatus<'_>> {
        let graph = self.scheduler.graph();
        let scheduled = graph.jobs().iter().filter(|job| {
            job.enabled()
                && job
                    .schedule()
                    .is_some_and(kortty_jobs::JobSchedule::enabled)
        });
        if self.active.is_empty() && scheduled.clone().next().is_none() {
            return None;
        }
        let running = self
            .active
            .iter()
            .filter_map(|(&id, active)| {
                Some(MenuRun {
                    id,
                    name: graph.job(&active.job)?.name(),
                    cancelling: active.cancellation.is_requested(),
                })
            })
            .collect();
        let mut upcoming: Vec<_> = scheduled
            .filter(|job| self.can_run() && !self.scheduler.is_running(job.id()))
            .filter_map(|job| Some((self.scheduler.scheduled_at(job.id())?, job)))
            .filter(|(due, _)| *due > now)
            .collect();
        upcoming.sort_by(|(time_a, job_a), (time_b, job_b)| {
            time_a.cmp(time_b).then_with(|| job_a.id().cmp(job_b.id()))
        });
        let upcoming = upcoming
            .into_iter()
            .take(limit)
            .map(|(due, job)| MenuDeadline {
                name: job.name(),
                due,
                local_time: self.local_time(due),
            })
            .collect();
        Some(MenuStatus { running, upcoming })
    }

    pub(crate) fn cancel_menu_run(&self, ticket: u64) -> bool {
        self.active.iter().any(|(&id, active)| {
            id.value() == ticket
                && !active.cancellation.is_requested()
                && self.scheduler.cancel(id).is_ok()
        })
    }

    pub(crate) fn cancel_firing_web_run(&self, id: RunId) {
        let _ = self.scheduler.cancel(id);
    }

    pub(crate) fn requires_awake(&self) -> bool {
        self.has_active_runs()
            || (self.execution_allowed
                && !self.draining
                && self.scheduler.graph().jobs().iter().any(|job| {
                    job.enabled()
                        && job
                            .schedule()
                            .is_some_and(kortty_jobs::JobSchedule::enabled)
                }))
    }

    /// Discards an unsaved draft after the user confirms the pending close.
    pub(crate) fn confirm_close(&self, view: &mut SchedulerView) -> bool {
        if !view.close_pending() {
            return false;
        }
        view.load_draft(self.selected_job(view));
        view.confirm = None;
        true
    }

    /// Stops new runs and signals cancellation to work that was already dispatched.
    pub fn begin_drain(&mut self) {
        self.draining = true;
        self.scheduler.begin_drain();
        self.cancel_runs();
    }

    fn cancel_runs(&mut self) {
        self.sync_cascades.clear();
        self.agent_cascade_root_blockers.clear();
        for &id in self.active.keys() {
            let _ = self.scheduler.cancel(id);
        }
        self.cancel_pending_starts(SystemTime::now());
    }

    /// The desktop dispatches a runtime event once, independently of open views.
    pub(crate) fn update_runtime(&mut self, message: Message) -> Vec<RunRequest<SshCommand>> {
        self.update_runtime_at(message, SystemTime::now())
    }

    pub(crate) fn update_runtime_at(
        &mut self,
        message: Message,
        now: SystemTime,
    ) -> Vec<RunRequest<SshCommand>> {
        let (message, session_journals) = match message {
            Message::FinishedWithSessionJournals(completion, journals) => {
                (Message::Finished(completion), journals)
            }
            message => (message, Vec::new()),
        };
        let result = match message {
            Message::Tick => {
                if !self.flush(now) || !self.can_run() {
                    return Vec::new();
                }
                self.scheduler.tick(now).map_err(|error| error.to_string())
            }
            Message::Finished(completion) => {
                let existing_rsync = self.rsync_runs.finish(completion.id);
                match self
                    .scheduler
                    .complete(completion.id, completion.outcome, now)
                {
                    Ok(batch) => {
                        debug_assert!(self.firing_reviews.for_run(completion.id).next().is_none());
                        let swarm_chat = self
                            .active
                            .remove(&completion.id)
                            .and_then(|active| active.swarm_chat);
                        return self.receive(
                            batch,
                            Some(FinishedRun {
                                completion,
                                swarm_chat,
                                session_journals,
                                existing_rsync,
                            }),
                            now,
                        );
                    }
                    Err(error) => Err(error.to_string()),
                }
            }
            _ => return Vec::new(),
        };
        match result {
            Ok(batch) => self.receive(batch, None, now),
            Err(error) => {
                self.status = Some(error);
                Vec::new()
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn initialize_schedules_at(
        &mut self,
        zone: TimeZone,
        now: SystemTime,
    ) -> Result<(), String> {
        self.scheduler
            .initialize_schedules(zone.clone(), now)
            .map_err(|error| error.to_string())?;
        self.zone = zone;
        Ok(())
    }

    pub(crate) fn update_view(
        &mut self,
        view: &mut SchedulerView,
        message: Message,
    ) -> Vec<RunRequest<SshCommand>> {
        self.sync_view(view);
        if view.close_pending() && !matches!(message, Message::Dismiss) {
            return Vec::new();
        }
        let now = SystemTime::now();
        let result = match message {
            Message::Confirm => self.confirm(view, now),
            Message::Run(cascade) if self.can_request_run(view) => self.ask_run(view, cascade, now),
            message => {
                self.apply_view(view, message, now);
                self.sync_view(view);
                return Vec::new();
            }
        };
        let requests = match result {
            Ok(batch) => self.receive(batch, None, now),
            Err(error) => {
                view.status = Some(error);
                Vec::new()
            }
        };
        self.sync_view(view);
        requests
    }

    pub(crate) fn confirmed_job_deletion<'a>(&self, view: &'a SchedulerView) -> Option<&'a JobId> {
        let Some(Confirmation::Delete(job, _)) = &view.confirm else {
            return None;
        };
        (view.graph_revision == self.graph_revision && self.can_edit() && self.draft_matches(view))
            .then_some(job)
    }

    pub(crate) fn selected_run_detail_text(
        &self,
        view: &SchedulerView,
        run: RunId,
        session_journal_root: Option<&Path>,
    ) -> Option<String> {
        if self.draining
            || view.confirm.is_some()
            || view.hosted_close.is_some()
            || view.selected_journal_run() != Some(run)
            || !view.journal_record_visible(self, run)
        {
            return None;
        }
        let record = self.store.journal_record(run)?;
        let linked =
            session_journal_root.and_then(|root| Some((root, view.journal_details(run, root)?)));
        let sources = run_detail::Sources::for_run(self, record, linked)?;
        view.run_detail
            .as_ref()
            .filter(|editor| editor.matches(run, &sources))?
            .selection()
    }

    fn apply_view(&mut self, view: &mut SchedulerView, message: Message, now: SystemTime) {
        let edits_sftp_authority = matches!(
            &message,
            Message::ScheduleEnabled(_)
                | Message::Dependency(..)
                | Message::Mode(_)
                | Message::Weekday(..)
                | Message::AllWeekdays(_)
                | Message::AddFixedTimes
                | Message::RemoveFixedTime(_)
                | Message::SyncStanding(_)
                | Message::SyncReplaceChanged(_)
                | Message::Edit(
                    Field::From
                        | Field::Until
                        | Field::WindowStart
                        | Field::WindowEnd
                        | Field::Interval
                        | Field::Age
                        | Field::SyncStandingDays
                        | Field::SyncStandingFirings
                        | Field::SyncStandingActions
                        | Field::SyncStandingMib,
                    _,
                )
        );
        if edits_sftp_authority && view.sync_review.discard_standing() {
            view.status =
                Some("The recurring SFTP settings changed. Review the folders again.".into());
        }
        match message {
            Message::Page(page) => {
                view.page = page;
                if page != Page::Action
                    && view
                        .agent_review
                        .as_ref()
                        .is_some_and(|review| review.model.is_some())
                {
                    view.agent_review = None;
                }
                if page != Page::Journal {
                    view.journal_search.collapse();
                    view.journal_retention_java.close();
                }
                self.refresh_diagram(view);
            }
            Message::ResizeJobs(event) => view.layout.jobs.resize(event.split, event.ratio),
            Message::ResizeJournal(event) => view.layout.journal.resize(event.split, event.ratio),
            Message::JournalSearch(search) if view.page == Page::Journal => {
                view.journal_search.set_text(search);
            }
            Message::JournalSearchMode(mode) if view.page == Page::Journal => {
                view.journal_search.set_mode(mode);
            }
            Message::JournalColumns if view.page == Page::Journal => {
                view.journal_search.toggle_columns();
            }
            Message::JournalColumn(column, enabled) if view.page == Page::Journal => {
                view.journal_search.set_column(column, enabled);
            }
            Message::JournalSort(column) if view.page == Page::Journal => {
                view.journal_sort.select(column);
            }
            Message::EditJournalRetention(input) if view.page == Page::Journal => {
                let input = input.chars().take(5).collect();
                view.journal_retention_draft = Some((self.journal_retention_days, input));
            }
            Message::New => {
                view.load_draft(None);
                view.page = Page::Job;
                view.confirm = None;
                view.status = None;
            }
            Message::Select(id) => self.select(view, &id),
            Message::Edit(field, value) => {
                view.draft.set(field, value);
                view.dirty = true;
            }
            Message::AdoptImportedArchive
                if self.can_edit()
                    && view.page == Page::Action
                    && view.draft.has_imported_archive() =>
            {
                let result = self.current_connection_store().and_then(|store| {
                    let trust = crate::ssh_connection::trust_store()
                        .map_err(|_| "Could not access the SSH host-key trust store.")?;
                    let policy = crate::clipboard_policy::ClipboardPolicy::load();
                    view.draft
                        .adopt_imported_archive(&store, &trust, &policy, &self.archive_passwords)
                        .map_err(str::to_owned)
                });
                match result {
                    Ok(()) => {
                        view.dirty = true;
                        let status = if view.draft.sudo != SudoMode::Disabled {
                            "Review and approve the exact privileged archive before saving."
                        } else {
                            "Review the native archive settings and save the job."
                        };
                        view.status = Some(status.into());
                    }
                    Err(error) => view.status = Some(error),
                }
            }
            Message::ImportedStagingSudo(mode)
                if self.can_edit()
                    && view.page == Page::Action
                    && !view.draft.has_imported_privileged() =>
            {
                view.draft.select_imported_sudo(mode);
            }
            Message::ImportedPrivilegedSudo(mode)
                if self.can_edit()
                    && view.page == Page::Action
                    && view.draft.has_imported_privileged() =>
            {
                view.draft.select_imported_sudo(mode);
            }
            Message::AdoptImportedPrivileged
                if self.can_edit()
                    && view.page == Page::Action
                    && view.draft.has_imported_privileged() =>
            {
                let result = self.current_connection_store().and_then(|store| {
                    let trust = crate::ssh_connection::trust_store()
                        .map_err(|_| "Could not access the SSH host-key trust store.")?;
                    let policy = crate::clipboard_policy::ClipboardPolicy::load();
                    view.draft
                        .adopt_imported_privileged(&store, &trust, &policy, &self.snippet_path)
                        .map_err(str::to_owned)
                });
                match result {
                    Ok(file_action) => {
                        view.dirty = true;
                        view.privileged_review = privileged::DraftReview::default();
                        view.status = Some(if file_action {
                            "Check the native file settings and approve the exact privileged action before enabling this job."
                        } else {
                            "Verify the native command settings and save the job before enabling it."
                        }.into());
                    }
                    Err(error) => view.status = Some(error),
                }
            }
            Message::AdoptImportedTransfer(mode)
                if self.can_edit() && view.page == Page::Action =>
            {
                match self.require_current_draft_groups(view).and_then(|()| {
                    view.draft
                        .adopt_imported_transfer(mode)
                        .map_err(str::to_owned)
                }) {
                    Ok(()) => {
                        view.dirty = true;
                        let status = match mode {
                            imported_transfer::Mode::Staged(_) => {
                                "Review and approve this privileged transfer before enabling the job."
                            }
                            _ => "Review the native transfer settings and save the job.",
                        };
                        view.status = Some(status.into());
                    }
                    Err(error) => view.status = Some(error),
                }
            }
            Message::ConfigureImportedRsync { .. }
            | Message::ConfigureImportedExistingRsync
            | Message::ConfigureImportedExistingRsyncFile
            | Message::ConfigureImportedMixedRsync
            | Message::ConfigureImportedPasswordlessRsync { .. }
            | Message::ConfigureImportedPasswordlessMixedRsync
                if self.can_edit() && view.page == Page::Action =>
            {
                let existing = matches!(
                    message,
                    Message::ConfigureImportedExistingRsync
                        | Message::ConfigureImportedExistingRsyncFile
                        | Message::ConfigureImportedPasswordlessRsync { existing: true, .. }
                );
                let mixed = matches!(
                    message,
                    Message::ConfigureImportedMixedRsync
                        | Message::ConfigureImportedPasswordlessMixedRsync
                );
                let directory = matches!(
                    message,
                    Message::ConfigureImportedExistingRsync
                        | Message::ConfigureImportedRsync { directory: true }
                        | Message::ConfigureImportedPasswordlessRsync {
                            directory: true,
                            ..
                        }
                );
                let elevation = if matches!(
                    message,
                    Message::ConfigureImportedPasswordlessRsync { .. }
                        | Message::ConfigureImportedPasswordlessMixedRsync
                ) {
                    rsync::Elevation::Passwordless
                } else {
                    rsync::Elevation::Ordinary
                };
                match self.require_current_draft_groups(view).and_then(|()| {
                    view.draft
                        .configure_imported_rsync(directory, existing, mixed, elevation)
                        .map_err(str::to_owned)
                }) {
                    Ok(()) => {
                        view.dirty = true;
                        view.status = Some(
                            "Choose the executable and paths, then review the rsync routes.".into(),
                        );
                    }
                    Err(error) => view.status = Some(error),
                }
            }
            Message::EditCommand(action) => {
                view.dirty |= action.is_edit();
                view.draft.edit_command(action);
            }
            Message::EditAgentGoal(action) => {
                view.dirty |= view.draft.edit_agent_goal(action);
            }
            Message::EditScriptArguments(action) => {
                view.dirty |= action.is_edit();
                view.draft.edit_script_arguments(action);
            }
            Message::EditArchiveSources(action) => {
                view.dirty |= action.is_edit();
                view.draft.edit_archive_sources(action);
            }
            Message::EditArchiveExcludes(action) => {
                view.dirty |= action.is_edit();
                view.draft.edit_archive_excludes(action);
            }
            Message::EditSyncExcludes(action) => {
                view.dirty |= action.is_edit();
                view.draft.edit_sync_excludes(action);
            }
            Message::EditRsyncSources(action) => {
                view.dirty |= view.draft.edit_rsync_sources(action);
            }
            Message::RsyncDestination(profile, destination) => {
                view.dirty |= view.draft.set_rsync_destination(profile, destination);
            }
            Message::RsyncExisting(enabled) => {
                view.dirty |= view.draft.choose_existing_rsync_destination(enabled);
            }
            Message::RsyncAllowVacantDirectory(enabled) => {
                if view
                    .draft
                    .set_rsync_allow_vacant_directory_download(enabled)
                {
                    view.rsync_review = rsync::Editor::default();
                    view.dirty = true;
                }
            }
            Message::RsyncAllowVacantFile(enabled) => {
                if view.draft.set_rsync_allow_vacant_file_download(enabled) {
                    view.rsync_review = rsync::Editor::default();
                    view.dirty = true;
                }
            }
            Message::RsyncAllowVacantServerFile(enabled) => {
                if view
                    .draft
                    .set_rsync_allow_vacant_server_file_upload(enabled)
                {
                    view.rsync_review = rsync::Editor::default();
                    view.dirty = true;
                }
            }
            Message::RsyncAllowVacantServerTree(enabled) => {
                if view
                    .draft
                    .set_rsync_allow_vacant_server_tree_upload(enabled)
                {
                    view.rsync_review = rsync::Editor::default();
                    view.dirty = true;
                }
            }
            Message::RsyncMixed(enabled) => {
                view.dirty |= view.draft.change_rsync_mixed(enabled);
            }
            Message::RsyncIncludeSourceName(include_source_name) => {
                view.dirty |= view.draft.change_rsync_existing(|settings| {
                    settings.map(|settings| rsync::ExistingDestination {
                        include_source_name,
                        ..settings
                    })
                });
            }
            Message::RsyncMirror(delete) => {
                view.dirty |= view.draft.change_rsync_existing(|settings| {
                    settings.map(|settings| rsync::ExistingDestination { delete, ..settings })
                });
            }
            Message::RsyncPasswordless(enabled) if self.can_edit() && view.page == Page::Action => {
                match view.draft.set_rsync_elevation(enabled) {
                    Ok(true) => {
                        view.rsync_review = rsync::Editor::default();
                        view.dirty = true;
                    }
                    Ok(false) => {}
                    Err(error) => view.status = Some(error.into()),
                }
            }
            Message::RsyncStanding(enabled) => {
                view.draft.set_rsync_standing(enabled);
                view.rsync_review = rsync::Editor::default();
                view.dirty = true;
            }
            Message::SyncStanding(enabled) => {
                view.draft.set_sync_standing(enabled);
                view.dirty = true;
            }
            Message::SyncReplaceChanged(enabled) => {
                view.draft.set_sync_replace_changed(enabled);
                view.dirty = true;
            }
            Message::SyncPreviewed(ticket, result) => {
                if let Some(status) = view.sync_review.finish(&ticket, result) {
                    view.status = Some(status);
                }
            }
            Message::ApproveSync => {
                if let Err(error) = self.approve_sync(view) {
                    view.status = Some(error);
                }
            }
            Message::PrivilegedReplaceExisting(enabled)
                if self.can_edit() && view.page == Page::Action =>
            {
                if view.draft.set_privileged_replace_existing(enabled) {
                    view.privileged_review = privileged::DraftReview::default();
                    view.dirty = true;
                }
            }
            Message::PrivilegedReplacementPreviewed(preview) => {
                self.finish_privileged_preview(view, preview);
            }
            Message::PrivilegedStanding(enabled)
                if self.can_edit() && view.page == Page::Action =>
            {
                if view.draft.set_privileged_standing(enabled) {
                    view.privileged_review = privileged::DraftReview::default();
                    view.dirty = true;
                }
            }
            Message::ApprovePrivileged => {
                if let Err(error) = self.approve_privileged(view) {
                    view.status = Some(error);
                }
            }
            Message::DiscardPrivileged => {
                view.privileged_review = privileged::DraftReview::default();
                view.status = None;
            }
            Message::DiscardSync => {
                view.sync_review = sync::DraftReview::default();
                view.status = None;
            }
            Message::RsyncProgramReviewed(ticket, result) => {
                if let Some(status) = view
                    .rsync_review
                    .complete(&ticket, result.map(|value| *value))
                {
                    view.status = Some(status);
                }
            }
            Message::ApproveRsyncProgram => {
                if let Err(error) = self.approve_rsync(view) {
                    view.status = Some(error);
                }
            }
            Message::DiscardRsyncProgram => {
                view.rsync_review = rsync::Editor::default();
                view.status = None;
            }
            Message::AgentProfile(profile) if self.agent_profiles.entries.contains(&profile) => {
                view.draft.select_agent_profile(profile.id);
                view.dirty = true;
            }
            Message::AgentProbe(probe) if view.draft.can_edit_agent_workflow() => {
                view.draft.agent_probe = probe;
                view.dirty = true;
            }
            Message::RequireFreshWeb(ticket, enabled) => {
                if let Err(error) = self.set_fresh_web_requirement(view, &ticket, enabled) {
                    view.status = Some(error);
                }
            }
            Message::EditUnattendedWebTopic(ticket, topic) => {
                if let Err(error) = self.edit_unattended_web_topic(view, &ticket, topic) {
                    view.status = Some(error);
                }
            }
            Message::EditUnattendedWebFollowups(ticket, enabled) => {
                if let Err(error) = self.edit_unattended_web_followups(view, &ticket, enabled) {
                    view.status = Some(error);
                }
            }
            Message::EditUnattendedWebEnvironment(ticket, enabled) => {
                if let Err(error) = self.edit_unattended_web_environment(view, &ticket, enabled) {
                    view.status = Some(error);
                }
            }
            Message::EditUnattendedWebRounds(ticket, enabled) => {
                if let Err(error) = self.edit_unattended_web_rounds(view, &ticket, enabled) {
                    view.status = Some(error);
                }
            }
            Message::EditUnattendedWebRoundPages(ticket, enabled) => {
                if let Err(error) = self.edit_unattended_web_round_pages(view, &ticket, enabled) {
                    view.status = Some(error);
                }
            }
            Message::ReviewUnattendedWeb(ticket, topic) => {
                if let Err(error) = self.review_unattended_web(view, &ticket, &topic) {
                    view.status = Some(error);
                }
            }
            Message::RemoveUnattendedWeb(ticket) => {
                if let Err(error) = self.remove_unattended_web(view, &ticket) {
                    view.status = Some(error);
                }
            }
            Message::AgentProposed(ticket, result) => {
                if let Err(error) = self.finish_agent_proposal(view, &ticket, result) {
                    view.status = Some(error);
                }
            }
            Message::ApproveAgent => {
                if let Err(error) = self.approve_agent_review(view, None) {
                    view.status = Some(error);
                }
            }
            Message::ApproveFiringRequest(ticket) | Message::ApproveProposalCommands(ticket) => {
                if let Err(error) = self.approve_agent_review(view, Some(&ticket)) {
                    view.status = Some(error);
                }
            }
            Message::DiscardAgentReview => view.discard_proposal_review(),
            Message::ArchiveFormat(format) => {
                if view.draft.select_archive_format(format) {
                    view.dirty = true;
                }
            }
            Message::ArchiveServerOnly(enabled) => {
                view.dirty |= view.draft.set_archive_server_only(enabled);
            }
            Message::ArchiveServerPath(preserve) => {
                if view.draft.set_archive_server_path(preserve) {
                    view.privileged_review = privileged::DraftReview::default();
                    view.dirty = true;
                }
            }
            Message::ArchiveReuseOutput(enabled) => {
                if view.draft.set_archive_reuse_output(enabled) {
                    view.privileged_review = privileged::DraftReview::default();
                    view.dirty = true;
                }
            }
            Message::ArchivePassword(choice) => {
                if self
                    .archive_passwords
                    .iter()
                    .any(|entry| entry.id == choice.id)
                {
                    view.dirty |= view.draft.select_archive_password(choice.id);
                }
            }
            Message::OpenCredentialVault => {}
            Message::Sudo(mode)
                if !view.draft.has_imported_archive()
                    && !view.draft.has_imported_transfer()
                    && !view.draft.has_imported_privileged() =>
            {
                view.draft.select_sudo(mode);
                view.dirty = true;
            }
            Message::CommandSource(source) => {
                view.draft.select_source(source);
                if view.baseline.is_none() && source.is_agent() {
                    view.draft
                        .suggest_agent_profile(self.agent_profiles.default.as_ref());
                }
                view.dirty = true;
            }
            Message::ConnectionSelected(_)
            | Message::ConnectionAdded(_)
            | Message::ConnectionRemoved(_)
            | Message::ConnectionGroupAdded(_)
            | Message::ConnectionGroupRemoved(_)
            | Message::RefreshConnectionGroups
                if view.draft.imported_route().is_some()
                    || view.draft.has_imported_privileged() => {}
            Message::ConnectionSelected(connection) if !view.draft.has_connection_groups() => {
                view.draft.select_connection(connection);
                view.dirty = true;
            }
            Message::ConnectionAdded(connection) if view.draft.has_connection_groups() => {
                self.change_connection_groups(view, |draft, store| {
                    group_change(draft.add_group_connection(connection, store))
                });
            }
            Message::ConnectionAdded(connection) => match view.draft.add_connection(connection) {
                Ok(changed) => view.dirty |= changed,
                Err(error) => view.status = Some(error.into()),
            },
            Message::ConnectionRemoved(id) if view.draft.has_connection_groups() => {
                self.change_connection_groups(view, |draft, store| {
                    draft.remove_group_connection(id, store)
                });
            }
            Message::ConnectionRemoved(id) => {
                view.dirty |= view.draft.remove_connection(id);
            }
            Message::ConnectionGroupAdded(group) if !view.draft.has_unresolved_imported_ai() => {
                self.change_connection_groups(view, |draft, store| {
                    group_change(draft.add_connection_group(group, store))
                });
            }
            Message::ConnectionGroupRemoved(group) => {
                self.change_connection_groups(view, |draft, store| {
                    draft.remove_connection_group(&group, store)
                });
            }
            Message::RefreshConnectionGroups => {
                self.change_connection_groups(view, |draft, store| {
                    group_change(draft.refresh_connection_groups(store))
                });
            }
            Message::ScriptLanguage(language) => {
                view.draft.script_language = language;
                view.dirty = true;
            }
            Message::SnippetSelected(choice) => {
                view.draft.snippet_id = Some(choice.id);
                view.dirty = true;
            }
            Message::Enabled(enabled) if !enabled || !view.draft.requires_import_review() => {
                view.draft.enabled = enabled;
                view.dirty = true;
            }
            Message::ScheduleEnabled(enabled) => {
                view.draft.schedule_enabled = enabled;
                view.dirty = true;
            }
            Message::RecursiveDelete(recursive) => {
                view.draft.set_recursive_delete(recursive);
                view.dirty = true;
            }
            Message::PrivilegedCopyTree(enabled) => {
                view.draft.set_privileged_copy_tree(enabled);
                view.dirty = true;
            }
            Message::VirtualTerminal(enabled) => {
                view.draft.virtual_terminal.enabled = enabled;
                view.dirty = true;
            }
            Message::JournalDetail(detail)
                if !view.automation_journal_denied
                    && self.journal_output_allowed
                    && view.draft.supports_journal_output() =>
            {
                view.draft.journal_detail = detail;
                view.dirty = true;
            }
            Message::AutomationJournal(event) => {
                if view.automation_journal_denied {
                    return;
                }
                view.draft.automation_journal.edit(event);
                view.dirty = true;
            }
            Message::DesktopNotices(settings) => {
                view.draft.desktop_notices = settings;
                view.dirty = true;
            }
            Message::AddWebhook(choice) if !view.draft.has_unresolved_imported_ai() => {
                let targets = &mut view.draft.webhook_targets;
                if targets.len() < webhooks::MAX_TARGETS
                    && self
                        .webhook_choices
                        .iter()
                        .any(|entry| entry.id == choice.id && entry.settings.enabled)
                    && targets.iter().all(|target| target.credential != choice.id)
                {
                    targets.push(webhooks::Target {
                        credential: choice.id,
                        format: webhooks::Format::default(),
                    });
                    view.dirty = true;
                }
            }
            Message::WebhookFormat(index, format) => {
                if let Some(target) = view.draft.webhook_targets.get_mut(index) {
                    target.format = format;
                    view.dirty = true;
                }
            }
            Message::RemoveWebhook(index) => {
                if index < view.draft.webhook_targets.len() {
                    view.draft.webhook_targets.remove(index);
                    view.dirty = true;
                }
            }
            Message::CaptureChanges(enabled) => {
                view.draft.virtual_terminal.capture_changes = enabled;
                view.dirty = true;
            }
            Message::LimitIsSuccess(enabled) => {
                view.draft.virtual_terminal.limit_is_success = enabled;
                view.dirty = true;
            }
            Message::Weekday(day, checked) => {
                view.draft.set_day(day, checked);
                view.dirty = true;
            }
            Message::AllWeekdays(checked) if view.page == Page::Job => {
                view.dirty |= view.draft.set_all_days(checked);
            }
            Message::FixedTimeInput(input) if view.page == Page::Job => {
                view.status = view.draft.set_fixed_input(input).err();
            }
            Message::AddFixedTimes if view.page == Page::Job => {
                match view.draft.add_fixed_times() {
                    Ok(changed) => {
                        view.dirty |= changed;
                        view.status = (!changed).then(|| "These times are already listed.".into());
                    }
                    Err(error) => view.status = Some(error),
                }
            }
            Message::SelectFixedTime(time) if view.page == Page::Job => {
                view.draft.select_fixed_time(time);
            }
            Message::RemoveFixedTime(time) if view.page == Page::Job => {
                view.dirty |= view.draft.remove_fixed_time(time);
            }
            Message::Dependency(id, checked) => {
                view.draft.set_dependency(id, checked);
                view.dirty = true;
            }
            Message::Mode(mode) => {
                view.draft.mode = mode;
                view.dirty = true;
            }
            Message::Save if self.can_edit() => {
                view.status = Some(self.save(view, now).unwrap_or_else(|error| error));
            }
            Message::Delete if self.can_edit() && self.draft_matches(view) => self.ask_delete(view),
            Message::SwitchChain if self.can_edit() && self.draft_matches(view) => {
                if let Some(job) = self.selected_job(view) {
                    view.confirm =
                        Some(Confirmation::SwitchChain(job.id().clone(), !job.enabled()));
                }
            }
            Message::CancelRun(id)
                if view.page == Page::Journal && view.journal_active_visible(self, id) =>
            {
                view.status = Some(match self.scheduler.cancel(id) {
                    Ok(()) => "Cancellation requested".into(),
                    Err(error) => error.to_string(),
                });
            }
            Message::Dismiss => {
                if view.confirm.take().is_none() {
                    view.journal_retention_java.close();
                }
            }
            Message::Close => {
                view.request_close();
            }
            Message::SelectRecord(id)
                if view.page == Page::Journal && view.journal_record_visible(self, id) =>
            {
                view.selected_run = Some(id);
                view.marked_records = BTreeSet::from([id]);
            }
            Message::MarkRecord(id, marked)
                if view.page == Page::Journal && view.journal_record_visible(self, id) =>
            {
                if marked {
                    view.marked_records.insert(id);
                    view.selected_run = Some(id);
                } else {
                    view.marked_records.remove(&id);
                }
            }
            Message::SelectActiveRun(id)
                if view.page == Page::Journal && view.journal_active_visible(self, id) =>
            {
                view.selected_run = Some(id);
                view.marked_records.clear();
            }
            Message::SelectRunDetail(run, action)
                if !action.is_edit() && view.confirm.is_none() =>
            {
                if let Some(editor) = view.run_detail.as_mut().filter(|editor| editor.run == run) {
                    editor.content.perform(action);
                }
            }
            Message::OpenRunDetailMenu(run, point) if view.confirm.is_none() => {
                if let Some(editor) = view.run_detail.as_mut().filter(|editor| editor.run == run) {
                    editor.menu = Some(point);
                }
            }
            Message::DismissRunDetailMenu(run) => view.dismiss_run_detail_menu(run),
            Message::DeleteRecord if view.page == Page::Journal && self.can_edit() => {
                let ids: Vec<_> = view.marked_records.iter().copied().collect();
                if view.journal_deletion_matches(self, &ids) {
                    view.confirm = Some(Confirmation::DeleteRecord(ids));
                }
            }
            Message::DiagramScope(scope) => {
                view.diagram.scope = scope;
                self.refresh_diagram(view);
            }
            Message::DiagramDirection => {
                view.diagram.toggle_direction();
                self.refresh_diagram(view);
            }
            Message::DiagramTheme => {
                view.diagram.toggle_theme();
                self.refresh_diagram(view);
            }
            Message::DiagramPath(path) => view.diagram.path = path,
            Message::DiagramExported(result) => {
                view.status = Some(match result {
                    Ok(path) => format!("Saved {}", path.display()),
                    Err(error) => format!("Diagram export failed ({error})"),
                });
            }
            Message::Zoom(factor) => view.diagram.zoom(factor),
            _ => {}
        }
    }

    fn ask_run(
        &mut self,
        view: &mut SchedulerView,
        cascade: bool,
        now: SystemTime,
    ) -> Result<RunBatch<SshCommand>, String> {
        if view.dirty {
            let status = self.save(view, now)?;
            // Reconcile this save before installing a dependency confirmation.
            self.sync_view(view);
            view.status = Some(status);
        }
        let Some(id) = view.draft.id.clone() else {
            return Ok(RunBatch::default());
        };
        if let Some(job) = self.scheduler.graph().job(&id) {
            job.action().require_executable()?;
        }
        if self.scheduler.graph().job(&id).is_some_and(|job| {
            matches!(&job.action().remote, Some(RemoteAction::Sync(copy)) if copy.standing.is_some())
        }) {
            return Err("Recurring SFTP copies start through an approved calendar or Chain.".into());
        }
        if self.scheduler.graph().job(&id).is_some_and(|job| {
            matches!(&job.action().remote, Some(RemoteAction::Privileged(transfer)) if transfer.standing.is_some())
        }) {
            return Err("Recurring protected replacements start through an approved calendar or Chain.".into());
        }
        let blockers = self
            .scheduler
            .blockers(&id, now)
            .map_err(|error| error.to_string())?;
        if cascade {
            let review = self.review_sync_cascade(&id)?;
            if let Some(reason) = review.root_unavailable() {
                return Err(reason.into());
            }
            if review.requires_confirmation() {
                view.confirm = Some(Confirmation::SyncCascade(review, blockers));
                return Ok(RunBatch::default());
            }
        }
        if blockers.is_empty() {
            self.start(&id, cascade, &blockers, now)
        } else {
            view.confirm = Some(Confirmation::Run(id, cascade, blockers));
            Ok(RunBatch::default())
        }
    }

    fn review_sync_cascade(&self, id: &JobId) -> Result<sync::CascadeReview, String> {
        let members = self
            .scheduler
            .cascade_preview(id)
            .map_err(|error| error.to_string())?;
        Ok(self.capture_sync_cascade(id, &members))
    }

    pub(crate) fn reviewed_cascade_root_blocked(&self, request: &RunRequest<SshCommand>) -> bool {
        if self.sync_cascades.is_empty() || request.cancellation.is_requested() {
            return false;
        }
        let Some(origin) = self.scheduler.active_cascade_origin(request) else {
            return false;
        };
        origin.is_root
            && self
                .sync_cascades
                .get(&origin.root_run)
                .is_some_and(|(_, accepted)| {
                    !self.can_run()
                        || !self.cascade_root_conditions_match(
                            &request.job,
                            accepted,
                            SystemTime::now(),
                        )
                })
    }

    fn cascade_root_conditions_match(
        &self,
        job: &JobId,
        accepted: &[Blocker],
        now: SystemTime,
    ) -> bool {
        self.scheduler
            .blockers(job, now)
            .is_ok_and(|blockers| blockers.iter().all(|blocker| accepted.contains(blocker)))
    }

    pub(crate) fn agent_predecessors_current(
        &self,
        request: &RunRequest<SshCommand>,
        now: SystemTime,
    ) -> bool {
        match request.trigger {
            RunTrigger::Manual => true,
            RunTrigger::Scheduled | RunTrigger::Dependency => self
                .scheduler
                .active_predecessor_blockers(request, now)
                .is_some_and(|blockers| blockers.is_empty()),
            RunTrigger::Cascade => {
                let Some(origin) = self.scheduler.active_cascade_origin(request) else {
                    return false;
                };
                if origin.is_root {
                    self.agent_cascade_root_blockers
                        .get(&origin.root_run)
                        .is_some_and(|accepted| {
                            self.cascade_root_conditions_match(&request.job, accepted, now)
                        })
                } else {
                    self.scheduler
                        .active_cascade_blockers(request, now)
                        .is_some_and(|blockers| blockers.is_empty())
                }
            }
        }
    }

    fn start(
        &mut self,
        id: &JobId,
        cascade: bool,
        accepted_blockers: &[Blocker],
        now: SystemTime,
    ) -> Result<RunBatch<SshCommand>, String> {
        let job = self.scheduler.graph().job(id);
        if let Some(job) = job {
            job.action().require_executable()?;
        }
        let track_cascade = cascade && job.is_some_and(|job| job.action().agent.is_some());
        let result = if cascade {
            self.scheduler.start_cascade(id, now)
        } else {
            self.scheduler.start_manual(id, now)
        };
        let batch = result.map_err(|error| error.to_string())?;
        if track_cascade && let Some(request) = batch.started.first() {
            self.agent_cascade_root_blockers
                .insert(request.id, accepted_blockers.to_vec());
        }
        Ok(batch)
    }

    fn confirm(
        &mut self,
        view: &mut SchedulerView,
        now: SystemTime,
    ) -> Result<RunBatch<SshCommand>, String> {
        match view.confirm.take() {
            Some(Confirmation::SyncCascade(reviewed, blockers))
                if self.can_run() && !view.dirty =>
            {
                let root = reviewed.root();
                let current = self.review_sync_cascade(root)?;
                if let Some(reason) = current.root_unavailable() {
                    return Err(reason.into());
                }
                let current_blockers = self
                    .scheduler
                    .blockers(root, now)
                    .map_err(|error| error.to_string())?;
                if current != reviewed
                    || (!current_blockers.is_empty() && current_blockers != blockers)
                {
                    view.confirm = Some(Confirmation::SyncCascade(current, current_blockers));
                    view.status =
                        Some("The cascade changed. Review it again before starting.".into());
                    return Ok(RunBatch::default());
                }
                let batch = self.start(root, true, &current_blockers, now)?;
                if let Some(request) = batch.started.first() {
                    self.sync_cascades
                        .insert(request.id, (reviewed, current_blockers));
                }
                return Ok(batch);
            }
            Some(Confirmation::Run(id, cascade, reviewed)) if self.can_run() && !view.dirty => {
                let current = self
                    .scheduler
                    .blockers(&id, now)
                    .map_err(|error| error.to_string())?;
                if !current.is_empty() && current != reviewed {
                    view.confirm = Some(Confirmation::Run(id, cascade, current));
                    return Ok(RunBatch::default());
                }
                return self.start(&id, cascade, &current, now);
            }
            Some(Confirmation::Delete(id, _)) if self.can_edit() && self.draft_matches(view) => {
                self.delete(view, &id, now)?;
                view.status = Some("Job deleted".into());
            }
            Some(Confirmation::SwitchChain(id, enabled))
                if self.can_edit() && self.draft_matches(view) =>
            {
                let mut next = self
                    .store
                    .restore_with_actions::<SshCommand>()
                    .map_err(|error| error.to_string())?;
                next.initialize_schedules(self.zone.clone(), now)
                    .map_err(|error| error.to_string())?;
                next.set_chain_enabled(&id, enabled, now)
                    .map_err(|error| error.to_string())?;
                Self::require_resolved_enabled_jobs(next.graph())?;
                self.store
                    .commit(next.graph(), &[])
                    .map_err(|error| error.to_string())?;
                self.scheduler = next;
                self.graph_changed();
                self.select(view, &id);
                view.status = Some(
                    if enabled {
                        "Chain enabled"
                    } else {
                        "Chain disabled"
                    }
                    .into(),
                );
            }
            Some(Confirmation::DeleteRecord(ids))
                if self.can_edit() && view.journal_deletion_matches(self, &ids) =>
            {
                let recordings: Vec<_> = ids
                    .iter()
                    .filter_map(|&id| self.store.journal_recording(id).map(|name| (id, name)))
                    .collect();
                let count = self
                    .store
                    .delete_records(&ids)
                    .map_err(|error| error.to_string())?;
                let mut cleanup_failed = false;
                for (id, recording) in recordings {
                    cleanup_failed |= virtual_terminal::remove_recording(
                        &self.recording_directory,
                        id,
                        recording,
                    )
                    .is_err();
                }
                self.store.apply_history(&mut self.scheduler);
                self.diagram_changed();
                view.marked_records.clear();
                if view.selected_run.is_some_and(|id| ids.contains(&id)) {
                    view.selected_run = None;
                }
                let mut status = if count == 1 {
                    "Journal entry deleted".into()
                } else {
                    format!("{count} journal entries deleted")
                };
                if cleanup_failed {
                    status.push_str(". Could not remove all files from job-recordings.");
                }
                view.status = Some(status);
            }
            _ => {}
        }
        Ok(RunBatch::default())
    }

    fn ask_delete(&self, view: &mut SchedulerView) {
        let Some(id) = view.draft.id.clone() else {
            return;
        };
        let dependents = self
            .scheduler
            .graph()
            .jobs()
            .iter()
            .filter(|job| job.dependencies().contains(&id))
            .map(|job| job.name().to_owned())
            .collect();
        view.confirm = Some(Confirmation::Delete(id, dependents));
    }

    fn approve_rsync(&self, view: &mut SchedulerView) -> Result<(), String> {
        let Some((approval, intent)) = view.rsync_review.take() else {
            return Ok(());
        };
        self.require_current_draft_groups(view)?;
        if !self.draft_matches(view)
            || view.draft.rsync_standing_intent()? != intent
            || intent
                .as_ref()
                .is_some_and(|intent| self.zone.iana_name() != Some(intent.zone.as_str()))
            || !view.draft.approve_rsync(approval)
        {
            return Err(
                "The rsync settings or saved routes changed. Review the transfer again.".into(),
            );
        }
        let status = if intent.is_some() {
            let id = view.draft.id.clone().map_or_else(new_id, Ok)?;
            let job = self.build_current_draft(view, id.clone())?;
            let grant = rsync::standing::Grant::new(&job, &self.zone, SystemTime::now())
                .map_err(|error| error.to_string())?;
            view.draft.set_rsync_standing_id(grant.approval.clone());
            view.rsync_review.grant = Some((id, grant));
            "Recurring rsync approved. Save the job to keep the approval."
        } else {
            "Rsync executable and routes approved. Save the job to keep the approval."
        };
        view.dirty = true;
        view.status = Some(status.into());
        Ok(())
    }

    fn save(&mut self, view: &mut SchedulerView, now: SystemTime) -> Result<String, String> {
        if !self.draft_matches(view) {
            return Err("This job changed in another view. Select it again before saving.".into());
        }
        let id = match view
            .draft
            .id
            .as_ref()
            .or_else(|| view.sync_review.approved_job())
            .or_else(|| view.privileged_review.approved_job())
            .or_else(|| view.agent_approval.as_ref().map(|approval| &approval.id))
            .or_else(|| view.rsync_review.grant.as_ref().map(|(id, _)| id))
        {
            Some(id) => id.clone(),
            None => new_id()?,
        };
        let replacement = self.build_current_draft(view, id.clone())?;
        let (replacement, sync_receipt) = self.sync_save(view, replacement)?;
        let (replacement, privileged_receipt) = self.save_privileged(view, replacement)?;
        let replacement = self.agent_save(view, replacement)?;
        let mut standing = None;
        if let Some(config) = replacement
            .action()
            .remote
            .as_ref()
            .and_then(RemoteAction::rsync_transfer)
            .and_then(|(transfer, _)| transfer.standing.as_ref())
        {
            if let Some((approved_job, grant)) = &view.rsync_review.grant {
                if *approved_job != id
                    || config.approval.as_deref() != Some(&grant.approval)
                    || !grant.matches(&replacement, &self.zone, now)
                {
                    return Err(
                        "The recurring rsync settings changed. Review the transfer again.".into(),
                    );
                }
                standing = Some(grant.clone());
            } else if replacement.enabled() {
                self.rsync_standing
                    .saved(&replacement, &self.zone, now)
                    .map_err(|error| error.to_string())?;
            }
        }
        let mut jobs = self
            .scheduler
            .graph()
            .jobs()
            .iter()
            .map(|job| {
                job.definition()
                    .with_action(job.action().clone())
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(index) = jobs.iter().position(|job| job.id() == &id) {
            jobs[index] = replacement;
        } else {
            jobs.push(replacement);
        }
        let graph = JobGraph::new(jobs).map_err(|error| error.to_string())?;
        if let Some(grant) = standing {
            if self.scheduler.is_running(&id) {
                return Err(
                    "Wait for the active rsync firing to finish before replacing its approval."
                        .into(),
                );
            }
            let approval = grant.approval.clone();
            let prior = self
                .rsync_standing
                .swap(&id, grant, |id| self.scheduler.graph().job(id).is_some())
                .map_err(|error| error.to_string())?;
            if let Err(error) = self.replace_graph(graph, now) {
                return if self.rsync_standing.restore(&id, &approval, prior).is_ok() {
                    Err(error)
                } else {
                    Err(format!(
                        "{error}. The previous recurring rsync approval could not be restored."
                    ))
                };
            }
        } else if let Some(approval) = sync_receipt.or(privileged_receipt) {
            let rollback = self
                .sync_store
                .save_approval(&id, approval, |id| self.scheduler.graph().job(id).is_some())
                .map_err(|_| "Could not save the private file-transfer approval.")?;
            if let Err(error) = self.replace_graph(graph, now) {
                return if self.sync_store.restore_approval(&id, rollback).is_ok() {
                    Err(error)
                } else {
                    Err(format!(
                        "{error}. The previous file-transfer approval could not be restored. Review the action again."
                    ))
                };
            }
        } else {
            self.replace_graph(graph, now)?;
        }
        self.select(view, &id);
        Ok("Job saved".into())
    }

    fn delete(
        &mut self,
        view: &mut SchedulerView,
        id: &JobId,
        now: SystemTime,
    ) -> Result<(), String> {
        let jobs = self
            .scheduler
            .graph()
            .jobs()
            .iter()
            .filter(|job| job.id() != id)
            .map(|job| {
                let mut definition = job.definition();
                definition
                    .dependencies
                    .retain(|predecessor| predecessor != id);
                definition
                    .with_action(job.action().clone())
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.replace_graph(JobGraph::new(jobs).map_err(|error| error.to_string())?, now)?;
        view.load_draft(self.scheduler.graph().jobs().first());
        self.refresh_diagram(view);
        Ok(())
    }

    fn replace_graph(
        &mut self,
        graph: JobGraph<SshCommand>,
        now: SystemTime,
    ) -> Result<(), String> {
        Self::require_resolved_enabled_jobs(&graph)?;
        let mut next = Scheduler::new(graph);
        next.initialize_schedules(self.zone.clone(), now)
            .map_err(|error| error.to_string())?;
        self.store
            .commit_with_actions(next.graph(), &[], &[])
            .map_err(|error| error.to_string())?;
        self.store.apply_history(&mut next);
        self.scheduler = next;
        self.graph_changed();
        Ok(())
    }

    fn require_resolved_enabled_jobs(graph: &JobGraph<SshCommand>) -> Result<(), String> {
        for job in graph.jobs().iter().filter(|job| job.enabled()) {
            job.action()
                .require_resolved_action()
                .map_err(|error| format!("Job {}: {error}", job.id()))?;
        }
        Ok(())
    }

    fn select(&self, view: &mut SchedulerView, id: &JobId) {
        if let Some(job) = self.scheduler.graph().job(id) {
            view.load_draft(Some(job));
            view.confirm = None;
            view.status = None;
            self.refresh_diagram(view);
        }
    }

    pub(crate) fn select_journal_job(&self, view: &mut SchedulerView, id: &JobId) {
        if self.scheduler.graph().job(id).is_none() || view.hosted_close.is_some() {
            return;
        }
        if view.dirty && view.draft.id.as_ref() != Some(id) {
            view.status = Some("Save or discard your draft before selecting another job.".into());
            return;
        }
        if !view.dirty {
            self.select(view, id);
        }
        view.page = Page::Job;
    }

    pub(crate) fn automation_job_name(&self, id: &JobId) -> Option<&str> {
        self.scheduler.graph().job(id).map(kortty_jobs::Job::name)
    }

    pub(crate) fn automation_journal_jobs(&self) -> impl Iterator<Item = (&JobId, &str)> {
        self.scheduler
            .graph()
            .jobs()
            .iter()
            .filter(|job| job.action().supports_automation_journal())
            .map(|job| (job.id(), job.name()))
    }

    fn can_run(&self) -> bool {
        self.execution_allowed && !self.draining && self.pending_records.is_empty()
    }

    fn swarm_chat_source(&self, request: &RunRequest<SshCommand>) -> Option<ActiveSwarmChat> {
        let action = request.action.agent.as_ref()?;
        let approval = action.approval.as_ref()?;
        if action.kind != agent::Kind::Swarm
            || !matches!(
                action.probe,
                agent::Probe::SwarmReadOnly | agent::Probe::SwarmIterative
            )
            || !action.probe.allows_trigger(request.trigger)
            || approval.profile.id != action.profile
            || !request
                .action
                .targets()
                .eq(approval.targets.iter().map(|target| &target.reference))
            || !self.agent_run_approved(request)
        {
            return None;
        }
        Some(ActiveSwarmChat {
            profile: action.profile.clone(),
            targets: request.action.targets().map(|target| target.id).collect(),
        })
    }

    fn can_request_run(&self, view: &SchedulerView) -> bool {
        self.can_run()
            && !view.draft.has_imported_archive()
            && !view.draft.has_imported_transfer()
            && !view.draft.has_imported_privileged()
            && if view.dirty {
                self.can_edit()
            } else {
                self.selected_job(view)
                    .is_some_and(|job| !self.scheduler.is_running(job.id()))
            }
    }

    pub(crate) fn can_browse_directory(&self) -> bool {
        self.execution_allowed && !self.draining
    }

    pub(crate) fn begin_automation_estimate(
        &self,
        view: &mut SchedulerView,
    ) -> Option<automation_journal::Request> {
        if self.draining
            || view.page != Page::Action
            || view.confirm.is_some()
            || view.automation_journal_denied
        {
            return None;
        }
        let subject = view.draft.automation_estimate_subject()?;
        view.draft
            .automation_journal
            .estimate
            .begin(subject, self.zone.clone(), SystemTime::now())
    }

    fn can_edit(&self) -> bool {
        !self.draining && !self.has_active_runs() && self.pending_records.is_empty()
    }

    fn selected_job(&self, view: &SchedulerView) -> Option<&kortty_jobs::Job<SshCommand>> {
        self.scheduler.graph().job(view.draft.id.as_ref()?)
    }

    fn draft_matches(&self, view: &SchedulerView) -> bool {
        let Some((definition, action)) = &view.baseline else {
            return view.draft.id.is_none();
        };
        self.selected_job(view)
            .is_some_and(|job| job.definition() == *definition && job.action() == action)
    }

    fn receive(
        &mut self,
        batch: RunBatch<SshCommand>,
        mut completion: Option<FinishedRun>,
        now: SystemTime,
    ) -> Vec<RunRequest<SshCommand>> {
        let changed = !batch.finished.is_empty() || !batch.started.is_empty();
        self.sync_cascades
            .retain(|&root, _| self.scheduler.has_cascade(root));
        self.agent_cascade_root_blockers
            .retain(|&root, _| self.scheduler.has_cascade(root));
        self.pending_records
            .extend(batch.finished.into_iter().map(|record| {
                let mut pending = PendingRecord {
                    record,
                    detail: None,
                    output: None,
                    recording: None,
                    session_journals: Vec::new(),
                    swarm_report: None,
                    swarm_chat_origin: None,
                };
                if let Some(FinishedRun {
                    completion: mut result,
                    swarm_chat,
                    session_journals,
                    existing_rsync,
                }) = completion.take_if(|run| run.completion.id == pending.record.id)
                {
                    let expected_report = result.swarm_report.is_some()
                        && pending.record.outcome != RunOutcome::Cancelled;
                    let report =
                        result.swarm_report.take().filter(|report| {
                            report.is_valid_for(&pending.record)
                                && self.scheduler.graph().job(&pending.record.job).is_some_and(
                                    |job| {
                                        let action = job.action();
                                        action.agent.as_ref().is_some_and(|agent| {
                                            matches!(
                                                agent.probe,
                                                agent::Probe::SwarmReadOnly
                                                    | agent::Probe::SwarmIterative
                                            )
                                        }) && action
                                            .targets()
                                            .map(|target| target.id)
                                            .eq(report.targets.iter().map(|target| target.target))
                                    },
                                )
                        });
                    if expected_report && report.is_none() {
                        result.append_detail("Could not save the Swarm report");
                    }
                    if self.swarm_chats_allowed
                        && let Some((source, _)) = swarm_chat
                            .zip(report.as_deref())
                            .filter(|(source, report)| source.matches(report))
                    {
                        match crate::private_files::nonce() {
                            Ok(nonce) => {
                                pending.swarm_chat_origin = Some(SwarmChatOrigin {
                                    nonce,
                                    profile: source.profile,
                                })
                            }
                            Err(_) => result
                                .append_detail("Could not identify the saved Swarm conversation"),
                        }
                    }
                    if valid_session_journal_ids(&session_journals) {
                        pending.session_journals = session_journals;
                    } else if !session_journals.is_empty() {
                        result.append_detail("Could not link the target Session Journals");
                    }
                    pending.detail = result
                        .detail
                        .as_deref()
                        .and_then(|detail| safe_detail(detail, existing_rsync));
                    if self.journal_output_allowed
                        && pending.record.outcome != RunOutcome::Cancelled
                        && pending.record.blockers.is_empty()
                        && self
                            .scheduler
                            .graph()
                            .job(&pending.record.job)
                            .is_some_and(|job| {
                                let action = job.action();
                                !action.journal_detail.is_off()
                                    && action.virtual_terminal.is_none()
                                    && action.supports_journal_output()
                            })
                    {
                        pending.output = result.output;
                    }
                    pending.recording = result.recording;
                    pending.swarm_report = report;
                }
                pending
            }));
        self.pending_starts.extend(batch.started);
        let starts = if self.flush(now) && self.can_run() {
            let starts = std::mem::take(&mut self.pending_starts);
            for run in &starts {
                let swarm_chat = self.swarm_chat_source(run);
                self.active.insert(
                    run.id,
                    Active {
                        job: run.job.clone(),
                        started_at: run.started_at,
                        trigger: run.trigger,
                        cancellation: run.cancellation.clone(),
                        swarm_chat,
                    },
                );
            }
            starts
        } else {
            Vec::new()
        };
        if changed || !starts.is_empty() {
            self.diagram_changed();
        }
        starts
    }

    fn flush(&mut self, now: SystemTime) -> bool {
        let has_records = !self.pending_records.is_empty();
        let mut skipped_swarm_chat = false;
        while !self.pending_records.is_empty() {
            let output: Vec<_> = self
                .pending_records
                .iter_mut()
                .take(MAX_JOURNAL_RECORDS)
                .filter_map(|item| {
                    let output = item.output.as_ref()?.current(None).cloned();
                    if output.is_none() {
                        item.output = None;
                    }
                    Some((item.record.id, output?))
                })
                .collect();
            let chunk = self.pending_records.iter().take(MAX_JOURNAL_RECORDS);
            let records: Vec<_> = chunk.clone().map(|item| item.record.clone()).collect();
            let details: Vec<_> = chunk
                .clone()
                .filter_map(|item| Some((item.record.id, item.detail.clone()?)))
                .collect();
            let recordings: Vec<_> = chunk
                .clone()
                .filter_map(|item| Some((item.record.id, item.recording?)))
                .collect();
            let session_journals: Vec<_> = chunk
                .clone()
                .filter(|item| !item.session_journals.is_empty())
                .map(|item| (item.record.id, item.session_journals.as_slice()))
                .collect();
            let swarm_reports: Vec<_> = chunk
                .clone()
                .filter_map(|item| Some((item.record.id, item.swarm_report.as_deref()?.clone())))
                .collect();
            let swarm_chat_origins: Vec<_> = chunk
                .filter(|_| self.swarm_chats_allowed)
                .filter_map(|item| Some((item.record.id, item.swarm_chat_origin.clone()?)))
                .collect();
            let previous: Vec<_> = self.store.journal_recordings().collect();
            let notices =
                notifications::State::prepare(&self.store, self.scheduler.graph(), &records);
            let deliveries =
                webhooks::State::prepare(&self.store, self.scheduler.graph(), &records);
            if let Err(error) = self.store.commit_finished_runs(
                self.scheduler.graph(),
                &records,
                FinishedRunMetadata {
                    details: &details,
                    output: &output,
                    recordings: &recordings,
                    session_journals: &session_journals,
                    swarm_reports: &swarm_reports,
                    swarm_chat_origins: &swarm_chat_origins,
                },
            ) {
                self.retry_at = now.checked_add(SAVE_RETRY);
                self.status = Some(format!(
                    "Could not save the run journal ({error}). New runs are paused."
                ));
                return false;
            }
            skipped_swarm_chat |= swarm_chat_origins
                .iter()
                .any(|(id, _)| self.store.pending_swarm_chat_seed(*id).is_none());
            self.notices.committed(notices);
            for ticket in self.webhooks.committed(deliveries) {
                self.record_webhook_result(ticket, kortty_jobs::WebhookOutcome::QueueFull);
            }
            for (id, name) in previous {
                if self.store.journal_recording(id).is_none() {
                    let _ = virtual_terminal::remove_recording(&self.recording_directory, id, name);
                }
            }
            self.pending_records.drain(..records.len());
        }
        if self.retry_at.take().is_some() {
            self.status = Some("Run journal saved".into());
        }
        if skipped_swarm_chat {
            self.status = Some(
                "The Swarm conversation queue is full. Open the saved report to continue.".into(),
            );
        }
        if has_records {
            self.expire_journal(now);
        }
        true
    }

    pub(crate) fn set_journal_retention(&mut self, days: u16, now: SystemTime) -> Option<usize> {
        self.journal_retention_days = days;
        self.expire_journal(now)
    }

    fn expire_journal(&mut self, now: SystemTime) -> Option<usize> {
        match self.prune_journal(now) {
            Ok(removed) => {
                self.journal_retention_error = None;
                Some(removed)
            }
            Err(error) => {
                self.journal_retention_error = Some(error);
                None
            }
        }
    }

    fn prune_journal(&mut self, now: SystemTime) -> Result<usize, String> {
        if self.journal_retention_days == 0 || !self.pending_records.is_empty() {
            return Ok(0);
        }
        let cutoff = journal_cutoff(now, &self.zone, self.journal_retention_days)
            .map_err(|error| format!("Could not calculate run journal retention ({error})"))?;
        let recordings: Vec<_> = self.store.journal_recordings().collect();
        let removed = self
            .store
            .prune_before(cutoff)
            .map_err(|error| format!("Could not apply run journal retention ({error})"))?;
        if removed == 0 {
            return Ok(0);
        }
        self.store.apply_history(&mut self.scheduler);
        self.diagram_changed();
        let mut cleanup_failed = false;
        for (id, recording) in recordings {
            if self.store.journal_recording(id).is_none() {
                cleanup_failed |=
                    virtual_terminal::remove_recording(&self.recording_directory, id, recording)
                        .is_err();
            }
        }
        if cleanup_failed {
            return Err(
                "Run journal retention could not remove all files from job-recordings.".into(),
            );
        }
        Ok(removed)
    }

    pub(crate) fn has_desktop_notices(&self) -> bool {
        !self.notices.is_empty()
    }

    pub(crate) fn take_desktop_notices(
        &mut self,
        now: Instant,
        allowed: bool,
    ) -> Vec<notifications::Notice> {
        self.notices.take(now, allowed, self.scheduler.graph())
    }

    pub(crate) fn next_webhook(&mut self) -> Option<webhooks::Delivery> {
        self.webhooks.next()
    }

    pub(crate) fn revoke_webhooks(
        &self,
        allowed: bool,
        settings: impl Fn(&str) -> Option<kortty_vault::WebhookSettings>,
    ) {
        self.webhooks
            .revoke(allowed, self.scheduler.graph(), settings);
    }

    pub(crate) fn webhook_summary(&self, run: RunId) -> Option<&str> {
        self.store.journal_detail(run)
    }

    pub(crate) fn start_webhook(
        &mut self,
        delivery: webhooks::Delivery,
        settings: kortty_vault::WebhookSettings,
    ) -> Option<Arc<std::sync::atomic::AtomicBool>> {
        self.store.journal_record(delivery.ticket.run)?;
        self.webhooks
            .start(delivery, settings, self.scheduler.graph())
    }

    pub(crate) fn finish_webhook(
        &mut self,
        ticket: webhooks::Ticket,
        outcome: kortty_jobs::WebhookOutcome,
    ) {
        if self.webhooks.finish(&ticket) {
            self.record_webhook_result(ticket, outcome);
        }
    }

    pub(crate) fn record_webhook_result(
        &mut self,
        ticket: webhooks::Ticket,
        outcome: kortty_jobs::WebhookOutcome,
    ) {
        if self.store.journal_record(ticket.run).is_some() {
            self.webhook_results
                .entry(ticket.run)
                .or_default()
                .entry(ticket.credential)
                .or_insert(outcome);
        }
    }

    pub(crate) fn save_webhook_results(&mut self, now: Instant) {
        self.webhook_results
            .retain(|run, _| self.store.journal_record(*run).is_some());
        if self.webhook_results.is_empty() || self.webhook_retry_at.is_some_and(|retry| retry > now)
        {
            return;
        }
        if self
            .store
            .record_webhook_results(&self.webhook_results)
            .is_ok()
        {
            self.webhook_results.clear();
            self.webhook_retry_at = None;
        } else {
            self.webhook_retry_at = now.checked_add(SAVE_RETRY);
            self.status = Some("Could not save webhook delivery results.".into());
        }
    }

    pub(crate) fn webhook_results_pending(&self) -> bool {
        !self.webhook_results.is_empty()
    }

    fn cancel_pending_starts(&mut self, now: SystemTime) {
        for request in std::mem::take(&mut self.pending_starts) {
            if let Ok(batch) = self
                .scheduler
                .complete(request.id, RunOutcome::Cancelled, now)
            {
                self.pending_records
                    .extend(batch.finished.into_iter().map(|record| PendingRecord {
                        record,
                        detail: None,
                        output: None,
                        recording: None,
                        session_journals: Vec::new(),
                        swarm_report: None,
                        swarm_chat_origin: None,
                    }));
            }
        }
        self.flush(now);
        self.diagram_changed();
    }

    fn graph_changed(&mut self) {
        self.rsync_runs.retain_current(self.scheduler.graph());
        self.graph_revision = self.graph_revision.wrapping_add(1);
        self.diagram_changed();
    }

    fn diagram_changed(&mut self) {
        self.diagram_revision = self.diagram_revision.wrapping_add(1);
    }

    fn refresh_diagram(&self, view: &mut SchedulerView) {
        view.diagram_revision = self.diagram_revision;
        if view.page == Page::Diagram {
            view.diagram
                .refresh(&self.scheduler, view.draft.id.as_ref());
        }
    }

    fn local_time(&self, instant: SystemTime) -> String {
        let Ok(timestamp) = instant.try_into() else {
            return "Unavailable".into();
        };
        let date_time = self.zone.to_datetime(timestamp);
        format!(
            "{} {:02}:{:02}:{:02}",
            date_time.date(),
            date_time.hour(),
            date_time.minute(),
            date_time.second()
        )
    }
}

impl SchedulerView {
    pub(crate) fn diagram_mermaid(&self) -> Option<String> {
        Some(self.diagram.preview.as_ref()?.mermaid.clone())
    }

    pub(crate) fn export_diagram(&self, format: Format) -> Task<Message> {
        let Some(preview) = &self.diagram.preview else {
            return Task::none();
        };
        let path = PathBuf::from(self.diagram.path.trim());
        let scene = Arc::clone(&preview.scene);
        let theme = self.diagram.theme;
        Task::future(async move {
            Message::DiagramExported(
                diagram_window::export(&path, &scene, theme, format)
                    .map(|()| path)
                    .map_err(|error| error.to_string()),
            )
        })
    }
}

fn journal_cutoff(now: SystemTime, zone: &TimeZone, days: u16) -> Result<SystemTime, jiff::Error> {
    let local = Timestamp::try_from(now)?.to_zoned(zone.clone());
    let cutoff = local
        .datetime()
        .checked_sub(Span::new().days(i64::from(days)))?;
    // Java retains the current offset when the resulting local time is repeated.
    let cutoff = OffsetConflict::PreferOffset
        .resolve(cutoff, local.offset(), zone.clone())?
        .compatible()?;
    Ok(SystemTime::from(cutoff.timestamp()))
}

fn safe_detail(detail: &str, existing_rsync: bool) -> Option<String> {
    let max_bytes = if existing_rsync {
        kortty_jobs::MAX_JOURNAL_DETAIL_BYTES
    } else {
        1024
    };
    let mut safe = String::new();
    for ch in detail.chars() {
        if ch.is_control()
            || matches!(ch, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            continue;
        }
        if safe.len() + ch.len_utf8() > max_bytes {
            break;
        }
        safe.push(ch);
    }
    (!safe.trim().is_empty()).then_some(safe)
}

fn group_change(result: Result<bool, &'static str>) -> (bool, Option<&'static str>) {
    match result {
        Ok(changed) => (changed, None),
        Err(error) => (false, Some(error)),
    }
}

fn new_id() -> Result<JobId, String> {
    use std::fmt::Write as _;
    let mut random = [0; 16];
    getrandom::fill(&mut random).map_err(|_| "Could not allocate a job identifier")?;
    let mut id = String::from("job_");
    for byte in random {
        let _ = write!(id, "{byte:02x}");
    }
    JobId::new(id).map_err(|error| error.to_string())
}
