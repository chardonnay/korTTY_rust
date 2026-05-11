use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum JobActionType {
    Command,
    SnippetScript,
    AiAgent,
    SftpUpload,
    SftpDownload,
    SftpSync,
    SftpDelete,
    SftpRename,
    SftpMkdir,
    SftpChmod,
    SftpChown,
    SftpCopyRemote,
    SftpArchive,
    RsyncSync,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum JobRunStatus {
    Running,
    Success,
    Failed,
    Blocked,
    Cancelled,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum JournalDetailMode {
    #[default]
    LimitedRedacted,
    Full,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RsyncDirection {
    Upload,
    Download,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SftpSyncDirection {
    Upload,
    Download,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum JobArchiveFormat {
    Zip,
    ZipPassword,
    Tar,
    TarBz2,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobSchedule {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub weekdays: Vec<String>,
    #[serde(default)]
    pub fixed_times: Vec<String>,
    pub active_from_date: Option<String>,
    pub active_until_date: Option<String>,
    pub window_start_time: Option<String>,
    pub window_end_time: Option<String>,
    pub interval_minutes: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobAction {
    pub action_type: JobActionType,
    pub command: Option<String>,
    pub snippet_id: Option<String>,
    #[serde(default)]
    pub snippet_arguments: Vec<String>,
    pub ai_prompt: Option<String>,
    pub ai_profile_id: Option<String>,
    #[serde(default)]
    pub ai_auto_approve_commands: bool,
    pub local_path: Option<String>,
    pub remote_path: Option<String>,
    pub remote_source_path: Option<String>,
    pub remote_destination_path: Option<String>,
    pub new_name: Option<String>,
    pub permissions: Option<String>,
    pub owner: Option<String>,
    pub group: Option<String>,
    pub sync_direction: Option<SftpSyncDirection>,
    #[serde(default)]
    pub use_sudo: bool,
    #[serde(default)]
    pub sudo_staging_enabled: bool,
    #[serde(default)]
    pub archive_source_paths: Vec<String>,
    #[serde(default)]
    pub archive_exclude_patterns: Vec<String>,
    pub archive_path: Option<String>,
    pub archive_format: Option<JobArchiveFormat>,
    pub archive_compression_level: Option<u32>,
    #[serde(default)]
    pub archive_download_after_create: bool,
    pub archive_download_local_path: Option<String>,
    pub encrypted_archive_password: Option<String>,
    pub rsync_direction: Option<RsyncDirection>,
    #[serde(default)]
    pub rsync_source_paths: Vec<String>,
    pub rsync_target_root: Option<String>,
    #[serde(default)]
    pub rsync_delete_enabled: bool,
}

impl Default for JobAction {
    fn default() -> Self {
        Self {
            action_type: JobActionType::Command,
            command: None,
            snippet_id: None,
            snippet_arguments: Vec::new(),
            ai_prompt: None,
            ai_profile_id: None,
            ai_auto_approve_commands: false,
            local_path: None,
            remote_path: None,
            remote_source_path: None,
            remote_destination_path: None,
            new_name: None,
            permissions: None,
            owner: None,
            group: None,
            sync_direction: None,
            use_sudo: false,
            sudo_staging_enabled: false,
            archive_source_paths: Vec::new(),
            archive_exclude_patterns: Vec::new(),
            archive_path: None,
            archive_format: None,
            archive_compression_level: None,
            archive_download_after_create: false,
            archive_download_local_path: None,
            encrypted_archive_password: None,
            rsync_direction: None,
            rsync_source_paths: Vec::new(),
            rsync_target_root: None,
            rsync_delete_enabled: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledJob {
    pub id: String,
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub host_key_verification_disabled: bool,
    pub connection_id: Option<String>,
    pub connection_display_name: Option<String>,
    #[serde(default)]
    pub target_connection_ids: Vec<String>,
    #[serde(default)]
    pub target_group_names: Vec<String>,
    pub working_directory: Option<String>,
    #[serde(default)]
    pub journal_detail_mode: JournalDetailMode,
    #[serde(default)]
    pub schedule: JobSchedule,
    #[serde(default)]
    pub action: JobAction,
    pub last_run_at: Option<String>,
    pub next_run_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl Default for ScheduledJob {
    fn default() -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: String::new(),
            enabled: true,
            host_key_verification_disabled: false,
            connection_id: None,
            connection_display_name: None,
            target_connection_ids: Vec::new(),
            target_group_names: Vec::new(),
            working_directory: None,
            journal_detail_mode: JournalDetailMode::LimitedRedacted,
            schedule: JobSchedule::default(),
            action: JobAction::default(),
            last_run_at: None,
            next_run_at: None,
            created_at: now.clone(),
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalEntry {
    pub id: String,
    pub run_id: String,
    pub job_id: String,
    pub target_connection_id: Option<String>,
    pub target_display_name: Option<String>,
    pub status: JobRunStatus,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub summary: String,
    #[serde(default)]
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PinnedHostKey {
    pub id: String,
    pub host: String,
    pub port: u16,
    pub algorithm: String,
    pub sha256_fingerprint: String,
    pub openssh_public_key: Option<String>,
    pub pinned_at: String,
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SudoCredential {
    pub id: String,
    pub job_id: String,
    pub target_connection_id: Option<String>,
    pub encrypted_password: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveJobSummary {
    pub run_id: String,
    pub job_id: String,
    pub job_name: String,
    pub status: JobRunStatus,
    pub target_display_name: Option<String>,
    pub started_at: String,
    #[serde(default)]
    pub cancellable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobSchedulerStatusSummary {
    #[serde(default)]
    pub active_jobs: Vec<ActiveJobSummary>,
    #[serde(default)]
    pub next_runs: Vec<ScheduledJob>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename = "jobScheduler", rename_all = "camelCase")]
pub struct JobSchedulerStateFile {
    #[serde(default)]
    pub jobs: Vec<ScheduledJob>,
    #[serde(default)]
    pub journal_entries: Vec<JournalEntry>,
    #[serde(default)]
    pub pinned_host_keys: Vec<PinnedHostKey>,
    #[serde(default)]
    pub sudo_credentials: Vec<SudoCredential>,
}

fn default_true() -> bool {
    true
}
