use crate::ai;
use crate::model::ai::{AiProfile, AiRequestPayload};
use crate::model::connection::{ConnectionProtocol, ConnectionSettings};
use crate::model::jobscheduler::{
    ActiveJobSummary, JobAction, JobActionType, JobArchiveFormat, JobRunStatus, JobSchedule,
    JobSchedulerStateFile, JobSchedulerStatusSummary, JournalDetailMode, JournalEntry,
    PinnedHostKey, RsyncDirection, ScheduledJob, SftpSyncDirection, SudoCredential,
};
use crate::model::settings::GlobalSettings;
use crate::model::snippet::Snippet;
use crate::persistence::xml_repository;
use crate::security::vault::Vault;
use crate::snippet_tools::{build_embedded_one_liner, SnippetOneLinerRequest};
use crate::ssh::session::SSHSession;
use crate::teamwork::sync::SyncService;
use anyhow::{anyhow, Context, Result};
use base64::Engine as _;
use chrono::{DateTime, Datelike, Days, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, watch};

const JOB_SCHEDULER_FILE: &str = "job-scheduler.xml";
const GLOBAL_SETTINGS_FILE: &str = "global-settings.json";
const AI_PROFILES_FILE: &str = "ai-profiles.json";
const SNIPPETS_FILE: &str = "snippets.json";
const SEARCH_DAYS: i64 = 370;
const COMMAND_TIMEOUT_SECS: u64 = 6 * 60 * 60;
const JOB_STATUS_EVENT: &str = "job-scheduler-status";

#[derive(Clone)]
pub struct JobSchedulerManager {
    inner: Arc<Mutex<JobSchedulerInner>>,
}

#[derive(Default)]
struct JobSchedulerInner {
    state: JobSchedulerStateFile,
    active_runs: HashMap<String, ActiveRun>,
}

struct ActiveRun {
    summary: ActiveJobSummary,
    cancel_tx: watch::Sender<bool>,
}

struct JournalRecord<'a> {
    run_id: &'a str,
    job: &'a ScheduledJob,
    connection: Option<&'a ConnectionSettings>,
    status: JobRunStatus,
    summary: &'a str,
    detail: &'a str,
    finished_at: Option<String>,
}

struct RunFinish<'a> {
    app: &'a AppHandle,
    run_id: &'a str,
    job: &'a ScheduledJob,
    status: JobRunStatus,
    summary: &'a str,
    detail: &'a str,
    target: Option<&'a ConnectionSettings>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostKeyProbeResult {
    pub host: String,
    pub port: u16,
    pub algorithm: String,
    pub sha256_fingerprint: String,
    pub openssh_public_key: String,
}

impl JobSchedulerManager {
    pub fn new() -> Self {
        let state = load_state().unwrap_or_else(|error| {
            tracing::warn!(error = %error, "Failed to load job scheduler state");
            JobSchedulerStateFile::default()
        });
        Self {
            inner: Arc::new(Mutex::new(JobSchedulerInner {
                state,
                active_runs: HashMap::new(),
            })),
        }
    }

    pub fn jobs(&self) -> Result<Vec<ScheduledJob>> {
        let mut jobs = self.lock()?.state.jobs.clone();
        recompute_next_runs(&mut jobs);
        jobs.sort_by_key(|job| job.name.to_lowercase());
        Ok(jobs)
    }

    pub fn save_job(&self, mut job: ScheduledJob) -> Result<ScheduledJob> {
        normalize_job(&mut job);
        job.next_run_at = next_run_after(&job.schedule, Local::now()).map(|date| date.to_rfc3339());
        let mut inner = self.lock()?;
        if let Some(position) = inner
            .state
            .jobs
            .iter()
            .position(|candidate| candidate.id == job.id)
        {
            inner.state.jobs[position] = job.clone();
        } else {
            inner.state.jobs.push(job.clone());
        }
        save_state(&inner.state)?;
        Ok(job)
    }

    pub fn delete_job(&self, id: &str) -> Result<()> {
        let mut inner = self.lock()?;
        inner.state.jobs.retain(|job| job.id != id);
        inner
            .state
            .journal_entries
            .retain(|entry| entry.job_id != id);
        inner
            .state
            .sudo_credentials
            .retain(|credential| credential.job_id != id);
        save_state(&inner.state)
    }

    pub fn journal(&self, job_id: Option<&str>) -> Result<Vec<JournalEntry>> {
        let mut entries = self.lock()?.state.journal_entries.clone();
        if let Some(job_id) = job_id.map(str::trim).filter(|value| !value.is_empty()) {
            entries.retain(|entry| entry.job_id == job_id);
        }
        entries.sort_by(|left, right| right.started_at.cmp(&left.started_at));
        Ok(entries)
    }

    pub fn clear_journal(&self, job_id: Option<&str>) -> Result<()> {
        let mut inner = self.lock()?;
        if let Some(job_id) = job_id.map(str::trim).filter(|value| !value.is_empty()) {
            inner
                .state
                .journal_entries
                .retain(|entry| entry.job_id != job_id);
        } else {
            inner.state.journal_entries.clear();
        }
        save_state(&inner.state)
    }

    pub fn status_summary(&self) -> Result<JobSchedulerStatusSummary> {
        let inner = self.lock()?;
        let mut next_runs = inner
            .state
            .jobs
            .iter()
            .filter(|job| job.enabled && job.next_run_at.is_some())
            .cloned()
            .collect::<Vec<_>>();
        next_runs.sort_by(|left, right| left.next_run_at.cmp(&right.next_run_at));
        next_runs.truncate(8);
        Ok(JobSchedulerStatusSummary {
            active_jobs: inner
                .active_runs
                .values()
                .map(|run| run.summary.clone())
                .collect(),
            next_runs,
        })
    }

    pub fn pinned_host_keys(&self) -> Result<Vec<PinnedHostKey>> {
        let mut keys = self.lock()?.state.pinned_host_keys.clone();
        keys.sort_by(|left, right| {
            left.host
                .cmp(&right.host)
                .then_with(|| left.port.cmp(&right.port))
        });
        Ok(keys)
    }

    pub fn pin_host_key(
        &self,
        probe: HostKeyProbeResult,
        source: Option<String>,
    ) -> Result<PinnedHostKey> {
        let mut inner = self.lock()?;
        let key = PinnedHostKey {
            id: uuid::Uuid::new_v4().to_string(),
            host: probe.host,
            port: probe.port,
            algorithm: probe.algorithm,
            sha256_fingerprint: probe.sha256_fingerprint,
            openssh_public_key: Some(probe.openssh_public_key),
            pinned_at: Utc::now().to_rfc3339(),
            source,
        };
        inner
            .state
            .pinned_host_keys
            .retain(|existing| !(same_host_port(existing, &key.host, key.port)));
        inner.state.pinned_host_keys.push(key.clone());
        save_state(&inner.state)?;
        Ok(key)
    }

    pub fn delete_pinned_host_key(&self, id: &str) -> Result<()> {
        let mut inner = self.lock()?;
        inner.state.pinned_host_keys.retain(|key| key.id != id);
        save_state(&inner.state)
    }

    pub fn save_sudo_credential(&self, credential: SudoCredential) -> Result<SudoCredential> {
        let mut credential = credential;
        let now = Utc::now().to_rfc3339();
        if credential.id.trim().is_empty() {
            credential.id = uuid::Uuid::new_v4().to_string();
        }
        if credential.created_at.trim().is_empty() {
            credential.created_at = now.clone();
        }
        credential.updated_at = now;
        let mut inner = self.lock()?;
        if let Some(position) = inner
            .state
            .sudo_credentials
            .iter()
            .position(|existing| existing.id == credential.id)
        {
            inner.state.sudo_credentials[position] = credential.clone();
        } else {
            inner.state.sudo_credentials.push(credential.clone());
        }
        save_state(&inner.state)?;
        Ok(credential)
    }

    pub fn delete_sudo_credential(&self, id: &str) -> Result<()> {
        let mut inner = self.lock()?;
        inner
            .state
            .sudo_credentials
            .retain(|credential| credential.id != id);
        save_state(&inner.state)
    }

    pub fn run_job_now(
        &self,
        app: AppHandle,
        job_id: &str,
        vault: Option<tauri::State<'_, Vault>>,
    ) -> Result<String> {
        let job = {
            let inner = self.lock()?;
            inner
                .state
                .jobs
                .iter()
                .find(|job| job.id == job_id)
                .cloned()
                .ok_or_else(|| anyhow!("Scheduled job not found"))?
        };
        let run_id = uuid::Uuid::new_v4().to_string();
        let (cancel_tx, cancel_rx) = watch::channel(false);
        self.register_active_run(&run_id, &job, cancel_tx)?;
        let manager = self.clone();
        let vault_unlocked = vault
            .as_ref()
            .map(|vault| vault.is_unlocked().unwrap_or(false))
            .unwrap_or(false);
        let spawned_run_id = run_id.clone();
        tokio::spawn(async move {
            let result = manager
                .execute_job_run(&app, spawned_run_id.clone(), job, cancel_rx, vault_unlocked)
                .await;
            if let Err(error) = result {
                tracing::warn!(error = %error, "Job scheduler run failed");
            }
            let _ = manager.emit_status(&app);
        });
        Ok(run_id)
    }

    pub fn cancel_run(&self, run_id: &str) -> Result<()> {
        let inner = self.lock()?;
        let run = inner
            .active_runs
            .get(run_id)
            .ok_or_else(|| anyhow!("Job run not found"))?;
        let _ = run.cancel_tx.send(true);
        Ok(())
    }

    pub fn drain_shutdown(&self) -> Result<usize> {
        let inner = self.lock()?;
        for run in inner.active_runs.values() {
            let _ = run.cancel_tx.send(true);
        }
        Ok(inner.active_runs.len())
    }

    fn register_active_run(
        &self,
        run_id: &str,
        job: &ScheduledJob,
        cancel_tx: watch::Sender<bool>,
    ) -> Result<()> {
        let mut inner = self.lock()?;
        inner.active_runs.insert(
            run_id.to_string(),
            ActiveRun {
                summary: ActiveJobSummary {
                    run_id: run_id.to_string(),
                    job_id: job.id.clone(),
                    job_name: non_blank(&job.name, "Unnamed job").to_string(),
                    status: JobRunStatus::Running,
                    target_display_name: job.connection_display_name.clone(),
                    started_at: Utc::now().to_rfc3339(),
                    cancellable: true,
                },
                cancel_tx,
            },
        );
        Ok(())
    }

    async fn execute_job_run(
        &self,
        app: &AppHandle,
        run_id: String,
        job: ScheduledJob,
        cancel_rx: watch::Receiver<bool>,
        vault_unlocked: bool,
    ) -> Result<()> {
        let targets = resolve_job_targets(&job)?;
        if targets.is_empty() {
            self.finish_run(RunFinish {
                app,
                run_id: &run_id,
                job: &job,
                status: JobRunStatus::Blocked,
                summary: "No target connection was resolved.",
                detail: "",
                target: None,
            })?;
            return Ok(());
        }

        let mut final_status = JobRunStatus::Success;
        let target_count = targets.len();
        for connection in targets {
            if *cancel_rx.borrow() {
                final_status = JobRunStatus::Cancelled;
                self.add_journal(JournalRecord {
                    run_id: &run_id,
                    job: &job,
                    connection: Some(&connection),
                    status: JobRunStatus::Cancelled,
                    summary: "Job run cancelled before this target started.",
                    detail: "",
                    finished_at: Some(Utc::now().to_rfc3339()),
                })?;
                continue;
            }

            let outcome = self
                .execute_job_for_connection(
                    &job,
                    &connection,
                    target_count,
                    cancel_rx.clone(),
                    vault_unlocked,
                )
                .await;
            let (status, summary, detail) = match outcome {
                Ok(detail) => (
                    JobRunStatus::Success,
                    "Job action completed.".to_string(),
                    detail,
                ),
                Err(JobRunError::Blocked(message)) => {
                    final_status = JobRunStatus::Blocked;
                    (JobRunStatus::Blocked, message, String::new())
                }
                Err(JobRunError::Cancelled) => {
                    final_status = JobRunStatus::Cancelled;
                    (
                        JobRunStatus::Cancelled,
                        "Job run cancelled.".to_string(),
                        String::new(),
                    )
                }
                Err(JobRunError::Failed(message)) => {
                    if !matches!(
                        final_status,
                        JobRunStatus::Blocked | JobRunStatus::Cancelled
                    ) {
                        final_status = JobRunStatus::Failed;
                    }
                    (JobRunStatus::Failed, message, String::new())
                }
            };
            self.add_journal(JournalRecord {
                run_id: &run_id,
                job: &job,
                connection: Some(&connection),
                status,
                summary: &summary,
                detail: &redact_detail(&job, &detail),
                finished_at: Some(Utc::now().to_rfc3339()),
            })?;
        }

        self.complete_active_run(&run_id, &job, final_status)?;
        self.emit_status(app)?;
        Ok(())
    }

    async fn execute_job_for_connection(
        &self,
        job: &ScheduledJob,
        connection: &ConnectionSettings,
        target_count: usize,
        cancel_rx: watch::Receiver<bool>,
        vault_unlocked: bool,
    ) -> Result<String, JobRunError> {
        if matches!(connection.connection_protocol, ConnectionProtocol::Mosh) {
            return Err(JobRunError::Blocked(
                "Mosh targets are not supported by JobScheduler and were blocked.".into(),
            ));
        }
        if !job.host_key_verification_disabled && !self.has_pinned_host_key(connection)? {
            return Err(JobRunError::Blocked(format!(
                "Pinned host key is required for {}:{} before this job can run.",
                connection.host, connection.port
            )));
        }
        if job.action.use_sudo && !vault_unlocked {
            return Err(JobRunError::Blocked(
                "Master password is locked; sudo-dependent jobs are blocked.".into(),
            ));
        }
        if job.action.encrypted_archive_password.is_some() && !vault_unlocked {
            return Err(JobRunError::Blocked(
                "Master password is locked; password-protected archive jobs are blocked.".into(),
            ));
        }

        let (tx, _rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let mut session = SSHSession::new(connection.clone());
        session
            .connect(tx)
            .await
            .map_err(|error| JobRunError::Failed(format!("SSH connect failed: {error}")))?;
        let detail = execute_action(job, connection, target_count, &session, cancel_rx).await;
        let _ = session.disconnect().await;
        detail
    }

    fn has_pinned_host_key(&self, connection: &ConnectionSettings) -> Result<bool, JobRunError> {
        Ok(self
            .lock()
            .map_err(|error| JobRunError::Failed(error.to_string()))?
            .state
            .pinned_host_keys
            .iter()
            .any(|key| same_host_port(key, &connection.host, connection.port)))
    }

    fn add_journal(&self, record: JournalRecord<'_>) -> Result<()> {
        let mut inner = self.lock()?;
        let now = Utc::now().to_rfc3339();
        inner.state.journal_entries.push(JournalEntry {
            id: uuid::Uuid::new_v4().to_string(),
            run_id: record.run_id.to_string(),
            job_id: record.job.id.clone(),
            target_connection_id: record.connection.map(|connection| connection.id.clone()),
            target_display_name: record.connection.map(connection_display_name),
            status: record.status,
            started_at: now,
            finished_at: record.finished_at,
            summary: record.summary.to_string(),
            detail: record.detail.to_string(),
        });
        apply_retention(&mut inner.state)?;
        save_state(&inner.state)
    }

    fn finish_run(&self, finish: RunFinish<'_>) -> Result<()> {
        self.add_journal(JournalRecord {
            run_id: finish.run_id,
            job: finish.job,
            connection: finish.target,
            status: finish.status.clone(),
            summary: finish.summary,
            detail: finish.detail,
            finished_at: Some(Utc::now().to_rfc3339()),
        })?;
        self.complete_active_run(finish.run_id, finish.job, finish.status)?;
        self.emit_status(finish.app)
    }

    fn complete_active_run(
        &self,
        run_id: &str,
        job: &ScheduledJob,
        final_status: JobRunStatus,
    ) -> Result<()> {
        let mut inner = self.lock()?;
        inner.active_runs.remove(run_id);
        if let Some(stored) = inner
            .state
            .jobs
            .iter_mut()
            .find(|stored| stored.id == job.id)
        {
            stored.last_run_at = Some(Utc::now().to_rfc3339());
            stored.next_run_at =
                next_run_after(&stored.schedule, Local::now()).map(|date| date.to_rfc3339());
            stored.updated_at = Utc::now().to_rfc3339();
        }
        if matches!(final_status, JobRunStatus::Cancelled) {
            tracing::info!(job_id = %job.id, run_id, "Job scheduler run cancelled");
        }
        save_state(&inner.state)
    }

    fn emit_status(&self, app: &AppHandle) -> Result<()> {
        let summary = self.status_summary()?;
        let _ = app.emit(JOB_STATUS_EVENT, summary);
        Ok(())
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, JobSchedulerInner>> {
        self.inner
            .lock()
            .map_err(|_| anyhow!("Job scheduler state is poisoned"))
    }
}

impl Default for JobSchedulerManager {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, thiserror::Error)]
enum JobRunError {
    #[error("{0}")]
    Failed(String),
    #[error("{0}")]
    Blocked(String),
    #[error("job run cancelled")]
    Cancelled,
}

async fn execute_action(
    job: &ScheduledJob,
    connection: &ConnectionSettings,
    target_count: usize,
    session: &SSHSession,
    cancel_rx: watch::Receiver<bool>,
) -> Result<String, JobRunError> {
    match job.action.action_type {
        JobActionType::Command => {
            let command = require_non_blank(job.action.command.as_deref(), "Command is required")?;
            run_remote_command(job, session, command, cancel_rx).await
        }
        JobActionType::SnippetScript => {
            let command = build_snippet_script_command(job)?;
            run_remote_command(job, session, &command, cancel_rx).await
        }
        JobActionType::AiAgent => run_ai_agent_job(job).await,
        JobActionType::SftpUpload => {
            upload_file(
                session,
                require_non_blank(job.action.local_path.as_deref(), "Local path is required")?,
                require_non_blank(job.action.remote_path.as_deref(), "Remote path is required")?,
            )
            .await
        }
        JobActionType::SftpDownload => {
            download_file(
                session,
                require_non_blank(job.action.remote_path.as_deref(), "Remote path is required")?,
                require_non_blank(job.action.local_path.as_deref(), "Local path is required")?,
            )
            .await
        }
        JobActionType::SftpSync => run_sftp_sync(session, &job.action).await,
        JobActionType::SftpDelete => {
            run_remote_command(
                job,
                session,
                &format!(
                    "rm -rf {}",
                    shell_quote(require_non_blank(
                        job.action.remote_path.as_deref(),
                        "Remote path is required"
                    )?)
                ),
                cancel_rx,
            )
            .await
        }
        JobActionType::SftpRename => {
            let source = require_non_blank(
                job.action
                    .remote_source_path
                    .as_deref()
                    .or(job.action.remote_path.as_deref()),
                "Source path is required",
            )?;
            let destination =
                if let Some(destination) = job.action.remote_destination_path.as_deref() {
                    destination.to_string()
                } else {
                    let new_name =
                        require_non_blank(job.action.new_name.as_deref(), "New name is required")?;
                    let source_path = Path::new(source);
                    source_path
                        .parent()
                        .map(|parent| parent.join(new_name).to_string_lossy().to_string())
                        .unwrap_or_else(|| new_name.to_string())
                };
            run_remote_command(
                job,
                session,
                &format!("mv {} {}", shell_quote(source), shell_quote(&destination)),
                cancel_rx,
            )
            .await
        }
        JobActionType::SftpMkdir => {
            run_remote_command(
                job,
                session,
                &format!(
                    "mkdir -p {}",
                    shell_quote(require_non_blank(
                        job.action.remote_path.as_deref(),
                        "Remote path is required"
                    )?)
                ),
                cancel_rx,
            )
            .await
        }
        JobActionType::SftpChmod => {
            run_remote_command(
                job,
                session,
                &format!(
                    "chmod {} {}",
                    require_non_blank(
                        job.action.permissions.as_deref(),
                        "Permissions are required"
                    )?,
                    shell_quote(require_non_blank(
                        job.action.remote_path.as_deref(),
                        "Remote path is required"
                    )?)
                ),
                cancel_rx,
            )
            .await
        }
        JobActionType::SftpChown => {
            let owner = require_non_blank(job.action.owner.as_deref(), "Owner is required")?;
            let owner_group = if let Some(group) = job
                .action
                .group
                .as_deref()
                .filter(|value| !value.trim().is_empty())
            {
                format!("{owner}:{group}")
            } else {
                owner.to_string()
            };
            run_remote_command(
                job,
                session,
                &format!(
                    "chown {} {}",
                    shell_quote(&owner_group),
                    shell_quote(require_non_blank(
                        job.action.remote_path.as_deref(),
                        "Remote path is required"
                    )?)
                ),
                cancel_rx,
            )
            .await
        }
        JobActionType::SftpCopyRemote => {
            run_remote_command(
                job,
                session,
                &format!(
                    "cp -a {} {}",
                    shell_quote(require_non_blank(
                        job.action.remote_source_path.as_deref(),
                        "Source path is required"
                    )?),
                    shell_quote(require_non_blank(
                        job.action.remote_destination_path.as_deref(),
                        "Destination path is required"
                    )?)
                ),
                cancel_rx,
            )
            .await
        }
        JobActionType::SftpArchive => {
            let command = build_archive_command(&job.action)?;
            run_remote_command(job, session, &command, cancel_rx).await
        }
        JobActionType::RsyncSync => run_rsync_sync(job, connection, target_count).await,
    }
}

async fn run_remote_command(
    job: &ScheduledJob,
    session: &SSHSession,
    command: &str,
    cancel_rx: watch::Receiver<bool>,
) -> Result<String, JobRunError> {
    let mut command = command.trim().to_string();
    if let Some(working_directory) = job
        .working_directory
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        command = format!("cd {} && {command}", shell_quote(working_directory));
    }
    let command = if job.action.use_sudo {
        format!("sudo -n sh -lc {}", shell_quote(&command))
    } else {
        command
    };
    let (tx, mut rx) = mpsc::unbounded_channel();
    let command_display = command.clone();
    let exec = session
        .exec_command_streaming(
            &command,
            tx,
            cancel_rx,
            Duration::from_secs(COMMAND_TIMEOUT_SECS),
            None,
            false,
        )
        .await
        .map_err(|error| {
            if error.to_string().contains("cancelled") {
                JobRunError::Cancelled
            } else {
                JobRunError::Failed(format!("Remote command failed: {error}"))
            }
        })?;
    while rx.try_recv().is_ok() {}
    if exec.cancelled {
        return Err(JobRunError::Cancelled);
    }
    if exec.timed_out {
        return Err(JobRunError::Failed("Remote command timed out.".into()));
    }
    if let Some(status) = exec.exit_status {
        if status != 0 {
            return Err(JobRunError::Failed(format!(
                "Remote command exited with status {status}."
            )));
        }
    }
    Ok(format!(
        "Command: {}\nExit: {:?}\nStdout:\n{}\nStderr:\n{}",
        command_display, exec.exit_status, exec.stdout, exec.stderr
    ))
}

async fn upload_file(
    session: &SSHSession,
    local_path: &str,
    remote_path: &str,
) -> Result<String, JobRunError> {
    const CHUNK: usize = 32 * 1024;
    let data = fs::read(local_path)
        .map_err(|error| JobRunError::Failed(format!("Failed to read local file: {error}")))?;
    let escaped = shell_quote(remote_path);
    for (index, chunk) in data.chunks(CHUNK).enumerate() {
        let b64 = base64::engine::general_purpose::STANDARD.encode(chunk);
        let redirect = if index == 0 { ">" } else { ">>" };
        let command = format!(
            "printf '%s' '{}' | {{ base64 -d 2>/dev/null || base64 -D; }} {} {}",
            b64, redirect, escaped
        );
        session.exec_command(&command).await.map_err(|error| {
            JobRunError::Failed(format!("Upload chunk {index} failed: {error}"))
        })?;
    }
    Ok(format!("Uploaded {local_path} to {remote_path}"))
}

async fn download_file(
    session: &SSHSession,
    remote_path: &str,
    local_path: &str,
) -> Result<String, JobRunError> {
    let command = format!("cat {} | base64", shell_quote(remote_path));
    let output = session
        .exec_command(&command)
        .await
        .map_err(|error| JobRunError::Failed(format!("Remote download failed: {error}")))?;
    let cleaned = output
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    let data = base64::engine::general_purpose::STANDARD
        .decode(cleaned)
        .map_err(|error| JobRunError::Failed(format!("Base64 decode failed: {error}")))?;
    if let Some(parent) = Path::new(local_path).parent() {
        fs::create_dir_all(parent).map_err(|error| {
            JobRunError::Failed(format!("Failed to create local directory: {error}"))
        })?;
    }
    fs::write(local_path, data)
        .map_err(|error| JobRunError::Failed(format!("Failed to write local file: {error}")))?;
    Ok(format!("Downloaded {remote_path} to {local_path}"))
}

async fn run_sftp_sync(session: &SSHSession, action: &JobAction) -> Result<String, JobRunError> {
    match action
        .sync_direction
        .clone()
        .unwrap_or(SftpSyncDirection::Upload)
    {
        SftpSyncDirection::Upload => {
            upload_file(
                session,
                require_non_blank(action.local_path.as_deref(), "Local path is required")?,
                require_non_blank(action.remote_path.as_deref(), "Remote path is required")?,
            )
            .await
        }
        SftpSyncDirection::Download => {
            download_file(
                session,
                require_non_blank(action.remote_path.as_deref(), "Remote path is required")?,
                require_non_blank(action.local_path.as_deref(), "Local path is required")?,
            )
            .await
        }
    }
}

async fn run_ai_agent_job(job: &ScheduledJob) -> Result<String, JobRunError> {
    let profile_id = require_non_blank(
        job.action.ai_profile_id.as_deref(),
        "AI profile id is required",
    )?;
    let prompt = require_non_blank(job.action.ai_prompt.as_deref(), "AI prompt is required")?;
    let profiles: Vec<AiProfile> = xml_repository::load_json(AI_PROFILES_FILE)
        .map_err(|error| JobRunError::Failed(error.to_string()))?
        .unwrap_or_default();
    let profile = profiles
        .into_iter()
        .find(|profile| profile.id == profile_id)
        .ok_or_else(|| JobRunError::Blocked("AI profile not found.".into()))?;
    let payload = AiRequestPayload {
        action: crate::model::ai::AiAction::Ask,
        profile_id: profile.id.clone(),
        selected_text: String::new(),
        connection_display_name: job.connection_display_name.clone(),
        response_language_code: None,
        user_prompt: Some(prompt.to_string()),
        conversation_context: None,
    };
    let result = ai::execute_request(&profile, &payload)
        .await
        .map_err(|error| JobRunError::Failed(format!("AI agent job failed: {error}")))?;
    Ok(result.content)
}

fn build_snippet_script_command(job: &ScheduledJob) -> Result<String, JobRunError> {
    let snippet_id = require_non_blank(job.action.snippet_id.as_deref(), "Snippet id is required")?;
    let snippets: Vec<Snippet> = xml_repository::load_json(SNIPPETS_FILE)
        .map_err(|error| JobRunError::Failed(error.to_string()))?
        .unwrap_or_default();
    let snippet = snippets
        .into_iter()
        .find(|snippet| snippet.id == snippet_id)
        .ok_or_else(|| JobRunError::Blocked("Snippet not found.".into()))?;
    let result = build_embedded_one_liner(&SnippetOneLinerRequest {
        snippet,
        arguments: job.action.snippet_arguments.clone(),
        global_variables: Vec::new(),
    })
    .map_err(|error| JobRunError::Failed(error.to_string()))?;
    Ok(result.line)
}

fn build_archive_command(action: &JobAction) -> Result<String, JobRunError> {
    let archive_path =
        require_non_blank(action.archive_path.as_deref(), "Archive path is required")?;
    let sources = action
        .archive_source_paths
        .iter()
        .map(|path| path.trim())
        .filter(|path| !path.is_empty())
        .collect::<Vec<_>>();
    if sources.is_empty() {
        return Err(JobRunError::Failed(
            "At least one archive source path is required.".into(),
        ));
    }
    let compression = action.archive_compression_level.unwrap_or(6).min(9);
    let quoted_sources = sources
        .iter()
        .map(|source| shell_quote(source))
        .collect::<Vec<_>>()
        .join(" ");
    let excludes_zip = action
        .archive_exclude_patterns
        .iter()
        .filter(|pattern| !pattern.trim().is_empty())
        .map(|pattern| format!("-x {}", shell_quote(pattern.trim())))
        .collect::<Vec<_>>()
        .join(" ");
    let excludes_tar = action
        .archive_exclude_patterns
        .iter()
        .filter(|pattern| !pattern.trim().is_empty())
        .map(|pattern| format!("--exclude={}", shell_quote(pattern.trim())))
        .collect::<Vec<_>>()
        .join(" ");
    let command = match action
        .archive_format
        .clone()
        .unwrap_or(JobArchiveFormat::Zip)
    {
        JobArchiveFormat::Zip => format!(
            "zip -r -{compression} {} {} {}",
            shell_quote(archive_path),
            excludes_zip,
            quoted_sources
        ),
        JobArchiveFormat::ZipPassword => format!(
            "zip -er -{compression} {} {} {}",
            shell_quote(archive_path),
            excludes_zip,
            quoted_sources
        ),
        JobArchiveFormat::Tar => format!(
            "tar -cf {} {} {}",
            shell_quote(archive_path),
            excludes_tar,
            quoted_sources
        ),
        JobArchiveFormat::TarBz2 => format!(
            "BZIP2=-{compression} tar -cjf {} {} {}",
            shell_quote(archive_path),
            excludes_tar,
            quoted_sources
        ),
    };
    Ok(command.trim().to_string())
}

async fn run_rsync_sync(
    job: &ScheduledJob,
    connection: &ConnectionSettings,
    target_count: usize,
) -> Result<String, JobRunError> {
    let settings: GlobalSettings = xml_repository::load_json(GLOBAL_SETTINGS_FILE)
        .map_err(|error| JobRunError::Failed(error.to_string()))?
        .unwrap_or_default();
    let rsync = settings
        .job_scheduler_rsync_path
        .unwrap_or_else(|| "rsync".into());
    let sources = job
        .action
        .rsync_source_paths
        .iter()
        .map(|path| path.trim())
        .filter(|path| !path.is_empty())
        .collect::<Vec<_>>();
    if sources.is_empty() {
        return Err(JobRunError::Failed(
            "Rsync source paths are required.".into(),
        ));
    }
    let target_root = require_non_blank(
        job.action.rsync_target_root.as_deref(),
        "Rsync target root is required",
    )?;
    let direction = job
        .action
        .rsync_direction
        .clone()
        .unwrap_or(RsyncDirection::Upload);
    let mut args = vec!["-a".to_string(), "--itemize-changes".to_string()];
    if job.action.rsync_delete_enabled {
        args.push("--delete".into());
    }
    let known_hosts_path = write_rsync_known_hosts(connection)?;
    args.push("-e".into());
    args.push(build_rsync_ssh_command(&known_hosts_path));
    match direction {
        RsyncDirection::Upload => {
            args.extend(sources.iter().map(|source| source.to_string()));
            args.push(format!(
                "{}@{}:{}",
                connection.username,
                connection.host,
                shell_remote_path(target_root)
            ));
        }
        RsyncDirection::Download => {
            let target_root = if target_count > 1 {
                PathBuf::from(target_root)
                    .join(safe_target_directory_name(connection))
                    .to_string_lossy()
                    .to_string()
            } else {
                target_root.to_string()
            };
            fs::create_dir_all(&target_root).map_err(|error| {
                JobRunError::Failed(format!("Failed to create rsync target directory: {error}"))
            })?;
            for source in &sources {
                args.push(format!(
                    "{}@{}:{}",
                    connection.username,
                    connection.host,
                    shell_remote_path(source)
                ));
            }
            args.push(target_root);
        }
    }
    let output_result = tokio::process::Command::new(&rsync)
        .args(&args)
        .output()
        .await;
    let _ = fs::remove_file(&known_hosts_path);
    let output = output_result
        .map_err(|error| JobRunError::Failed(format!("Failed to start rsync: {error}")))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        return Err(JobRunError::Failed(format!(
            "Rsync exited with status {:?}.\nStdout:\n{}\nStderr:\n{}",
            output.status.code(),
            stdout,
            stderr
        )));
    }
    Ok(format!(
        "Rsync completed.\nCommand: {} {}\nStdout:\n{}\nStderr:\n{}",
        rsync,
        args.join(" "),
        stdout,
        stderr
    ))
}

fn write_rsync_known_hosts(connection: &ConnectionSettings) -> Result<PathBuf, JobRunError> {
    let state = load_state().map_err(|error| {
        JobRunError::Failed(format!(
            "Failed to load pinned host keys for rsync: {error}"
        ))
    })?;
    let key = state
        .pinned_host_keys
        .iter()
        .find(|key| same_host_port(key, &connection.host, connection.port))
        .ok_or_else(|| {
            JobRunError::Blocked(format!(
                "Pinned host key is required for rsync target {}:{}.",
                connection.host, connection.port
            ))
        })?;
    let openssh_public_key = key
        .openssh_public_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            JobRunError::Blocked(
                "Pinned host key was created by an older KorTTY version without OpenSSH key material; probe and pin the host again before running rsync jobs."
                    .into(),
            )
        })?;
    let path =
        std::env::temp_dir().join(format!("kortty-rsync-known-hosts-{}", uuid::Uuid::new_v4()));
    let known_hosts_entry = format!(
        "[{}]:{} {}\n",
        connection.host, connection.port, openssh_public_key
    );
    fs::write(&path, known_hosts_entry).map_err(|error| {
        JobRunError::Failed(format!("Failed to create rsync known_hosts file: {error}"))
    })?;
    Ok(path)
}

fn build_rsync_ssh_command(known_hosts_path: &Path) -> String {
    format!(
        "ssh -o BatchMode=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile={}",
        shell_quote(&known_hosts_path.to_string_lossy())
    )
}

fn shell_remote_path(path: &str) -> String {
    path.replace('\'', "'\\''")
}

fn safe_target_directory_name(connection: &ConnectionSettings) -> String {
    let display = connection_display_name(connection);
    let safe = display
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    let safe = safe.trim_matches('_');
    let safe = if safe.is_empty() { "target" } else { safe };
    if connection.id.len() >= 8 {
        format!("{}-{}", safe, &connection.id[..8])
    } else {
        safe.to_string()
    }
}

fn require_non_blank<'a>(value: Option<&'a str>, message: &str) -> Result<&'a str, JobRunError> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| JobRunError::Failed(message.to_string()))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn next_run_after(schedule: &JobSchedule, after: DateTime<Local>) -> Option<DateTime<Local>> {
    if !schedule.enabled {
        return None;
    }
    let from_date = parse_date(schedule.active_from_date.as_deref());
    let until_date = parse_date(schedule.active_until_date.as_deref());
    let allowed_days = parse_weekdays(&schedule.weekdays);
    let fixed_times = parse_times(&schedule.fixed_times);
    let window_start = parse_time(schedule.window_start_time.as_deref()).unwrap_or(NaiveTime::MIN);
    let window_end = parse_time(schedule.window_end_time.as_deref())
        .unwrap_or_else(|| NaiveTime::from_hms_opt(23, 59, 0).expect("valid time"));
    let interval_minutes = schedule.interval_minutes.filter(|value| *value > 0);

    let start_day = after.date_naive();
    for day_offset in 0..SEARCH_DAYS {
        let day = start_day.checked_add_days(Days::new(day_offset as u64))?;
        if !is_allowed_date(day, from_date, until_date, &allowed_days) {
            continue;
        }
        let mut candidates = Vec::new();
        for time in &fixed_times {
            if is_inside_window(*time, window_start, window_end) {
                push_candidate(&mut candidates, day, *time, after);
            }
        }
        if let Some(interval) = interval_minutes {
            let mut cursor = NaiveDateTime::new(day, window_start);
            let end = if window_end < window_start {
                NaiveDateTime::new(day.checked_add_days(Days::new(1))?, window_end)
            } else {
                NaiveDateTime::new(day, window_end)
            };
            while cursor <= end {
                push_candidate(&mut candidates, cursor.date(), cursor.time(), after);
                cursor += chrono::Duration::minutes(i64::from(interval));
            }
        }
        if fixed_times.is_empty() && interval_minutes.is_none() {
            push_candidate(&mut candidates, day, window_start, after);
        }
        if let Some(candidate) = candidates
            .into_iter()
            .filter(|candidate| *candidate > after)
            .min()
        {
            return Some(candidate);
        }
    }
    None
}

fn push_candidate(
    candidates: &mut Vec<DateTime<Local>>,
    day: NaiveDate,
    time: NaiveTime,
    after: DateTime<Local>,
) {
    let local = NaiveDateTime::new(day, time);
    if let Some(candidate) = Local.from_local_datetime(&local).single() {
        if candidate > after {
            candidates.push(candidate);
        }
    }
}

fn is_allowed_date(
    day: NaiveDate,
    from_date: Option<NaiveDate>,
    until_date: Option<NaiveDate>,
    allowed_days: &HashSet<chrono::Weekday>,
) -> bool {
    if from_date.is_some_and(|from| day < from) {
        return false;
    }
    if until_date.is_some_and(|until| day > until) {
        return false;
    }
    allowed_days.is_empty() || allowed_days.contains(&day.weekday())
}

fn is_inside_window(time: NaiveTime, start: NaiveTime, end: NaiveTime) -> bool {
    if end < start {
        time >= start || time <= end
    } else {
        time >= start && time <= end
    }
}

fn parse_date(value: Option<&str>) -> Option<NaiveDate> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d").ok())
}

fn parse_time(value: Option<&str>) -> Option<NaiveTime> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| NaiveTime::parse_from_str(value, "%H:%M").ok())
}

fn parse_times(values: &[String]) -> Vec<NaiveTime> {
    let mut times = values
        .iter()
        .filter_map(|value| parse_time(Some(value)))
        .collect::<Vec<_>>();
    times.sort();
    times.dedup();
    times
}

fn parse_weekdays(values: &[String]) -> HashSet<chrono::Weekday> {
    values
        .iter()
        .filter_map(|value| match value.trim().to_uppercase().as_str() {
            "MONDAY" | "MON" => Some(chrono::Weekday::Mon),
            "TUESDAY" | "TUE" | "TUES" => Some(chrono::Weekday::Tue),
            "WEDNESDAY" | "WED" => Some(chrono::Weekday::Wed),
            "THURSDAY" | "THU" | "THURS" => Some(chrono::Weekday::Thu),
            "FRIDAY" | "FRI" => Some(chrono::Weekday::Fri),
            "SATURDAY" | "SAT" => Some(chrono::Weekday::Sat),
            "SUNDAY" | "SUN" => Some(chrono::Weekday::Sun),
            _ => None,
        })
        .collect()
}

pub async fn probe_host_key(host: &str, port: u16) -> Result<HostKeyProbeResult> {
    use russh::client;
    use russh::keys::HashAlg;

    struct ProbeHandler {
        captured: Arc<Mutex<Option<(String, String, String)>>>,
    }

    impl client::Handler for ProbeHandler {
        type Error = anyhow::Error;

        async fn check_server_key(
            &mut self,
            server_public_key: &russh::keys::PublicKey,
        ) -> Result<bool, Self::Error> {
            let fingerprint = server_public_key.fingerprint(HashAlg::Sha256).to_string();
            let openssh = server_public_key.to_openssh()?;
            let algorithm = server_public_key.algorithm().to_string();
            *self
                .captured
                .lock()
                .map_err(|_| anyhow!("Host-key probe state is poisoned"))? =
                Some((algorithm, fingerprint, openssh));
            Ok(true)
        }
    }

    let captured = Arc::new(Mutex::new(None));
    let config = Arc::new(client::Config {
        ..Default::default()
    });
    let addr = format!("{}:{}", host.trim(), port);
    let handle = client::connect(
        config,
        addr,
        ProbeHandler {
            captured: Arc::clone(&captured),
        },
    )
    .await
    .with_context(|| format!("Failed to probe host key for {}:{}", host.trim(), port))?;
    drop(handle);
    let (algorithm, fingerprint, openssh_public_key) = captured
        .lock()
        .map_err(|_| anyhow!("Host-key probe state is poisoned"))?
        .clone()
        .ok_or_else(|| anyhow!("Server did not provide a host key"))?;
    Ok(HostKeyProbeResult {
        host: host.trim().to_string(),
        port,
        algorithm,
        sha256_fingerprint: fingerprint,
        openssh_public_key,
    })
}

fn normalize_job(job: &mut ScheduledJob) {
    if job.id.trim().is_empty() {
        job.id = uuid::Uuid::new_v4().to_string();
    }
    let now = Utc::now().to_rfc3339();
    if job.created_at.trim().is_empty() {
        job.created_at = now.clone();
    }
    job.updated_at = now;
    job.name = job.name.trim().to_string();
}

fn recompute_next_runs(jobs: &mut [ScheduledJob]) {
    for job in jobs {
        job.next_run_at = next_run_after(&job.schedule, Local::now()).map(|date| date.to_rfc3339());
    }
}

fn resolve_job_targets(job: &ScheduledJob) -> Result<Vec<ConnectionSettings>, JobRunError> {
    let mut connections: Vec<ConnectionSettings> = xml_repository::load_json("connections.json")
        .map_err(|error| JobRunError::Failed(error.to_string()))?
        .unwrap_or_default();
    if let Ok(teamwork) = SyncService::all_teamwork_connections() {
        connections.extend(teamwork);
    }

    let mut wanted_ids = job.target_connection_ids.clone();
    if wanted_ids.is_empty() {
        if let Some(id) = job
            .connection_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            wanted_ids.push(id.to_string());
        }
    }
    if !job.target_group_names.is_empty() {
        let groups = job
            .target_group_names
            .iter()
            .map(|group| group.to_lowercase())
            .collect::<HashSet<_>>();
        for connection in &connections {
            if connection
                .group
                .as_deref()
                .is_some_and(|group| groups.contains(&group.to_lowercase()))
            {
                wanted_ids.push(connection.id.clone());
            }
        }
    }
    let wanted = wanted_ids.into_iter().collect::<HashSet<_>>();
    Ok(connections
        .into_iter()
        .filter(|connection| wanted.contains(&connection.id))
        .collect())
}

fn apply_retention(state: &mut JobSchedulerStateFile) -> Result<()> {
    let settings: GlobalSettings =
        xml_repository::load_json(GLOBAL_SETTINGS_FILE)?.unwrap_or_default();
    let retention_days = settings.job_scheduler_journal_retention_days.max(1);
    let cutoff = Utc::now() - chrono::Duration::days(i64::from(retention_days));
    state.journal_entries.retain(|entry| {
        DateTime::parse_from_rfc3339(&entry.started_at)
            .map(|date| date.with_timezone(&Utc) >= cutoff)
            .unwrap_or(true)
    });
    Ok(())
}

fn redact_detail(job: &ScheduledJob, detail: &str) -> String {
    if matches!(job.journal_detail_mode, JournalDetailMode::Full) {
        return detail.to_string();
    }
    let mut redacted = detail.to_string();
    for secret in [
        job.action.encrypted_archive_password.as_deref(),
        job.action.command.as_deref().filter(|_| false),
    ]
    .into_iter()
    .flatten()
    {
        if !secret.trim().is_empty() {
            redacted = redacted.replace(secret, "[redacted]");
        }
    }
    redacted
}

fn same_host_port(key: &PinnedHostKey, host: &str, port: u16) -> bool {
    key.host.eq_ignore_ascii_case(host.trim()) && key.port == port
}

fn connection_display_name(connection: &ConnectionSettings) -> String {
    if !connection.name.trim().is_empty() {
        connection.name.trim().to_string()
    } else {
        format!("{}@{}", connection.username, connection.host)
    }
}

fn non_blank<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.trim().is_empty() {
        fallback
    } else {
        value.trim()
    }
}

fn load_state() -> Result<JobSchedulerStateFile> {
    let mut state: JobSchedulerStateFile =
        xml_repository::load_xml(JOB_SCHEDULER_FILE)?.unwrap_or_default();
    recompute_next_runs(&mut state.jobs);
    Ok(state)
}

fn save_state(state: &JobSchedulerStateFile) -> Result<()> {
    xml_repository::save_xml(JOB_SCHEDULER_FILE, state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calculates_fixed_time_schedule() {
        let schedule = JobSchedule {
            enabled: true,
            fixed_times: vec!["10:30".into()],
            ..JobSchedule::default()
        };
        let after = Local
            .from_local_datetime(
                &NaiveDate::from_ymd_opt(2026, 5, 11)
                    .unwrap()
                    .and_hms_opt(10, 0, 0)
                    .unwrap(),
            )
            .single()
            .unwrap();

        let next = next_run_after(&schedule, after).unwrap();

        assert_eq!(next.time().format("%H:%M").to_string(), "10:30");
    }

    #[test]
    fn skips_weekdays_that_do_not_match() {
        let schedule = JobSchedule {
            enabled: true,
            weekdays: vec!["TUESDAY".into()],
            fixed_times: vec!["08:00".into()],
            ..JobSchedule::default()
        };
        let monday = Local
            .from_local_datetime(
                &NaiveDate::from_ymd_opt(2026, 5, 11)
                    .unwrap()
                    .and_hms_opt(9, 0, 0)
                    .unwrap(),
            )
            .single()
            .unwrap();

        let next = next_run_after(&schedule, monday).unwrap();

        assert_eq!(next.weekday(), chrono::Weekday::Tue);
    }

    #[test]
    fn archive_command_quotes_paths_and_excludes() {
        let action = JobAction {
            action_type: JobActionType::SftpArchive,
            archive_path: Some("/tmp/a b.zip".into()),
            archive_format: Some(JobArchiveFormat::Zip),
            archive_source_paths: vec!["/var/log".into()],
            archive_exclude_patterns: vec!["*.gz".into()],
            archive_compression_level: Some(7),
            ..JobAction::default()
        };

        let command = build_archive_command(&action).unwrap();

        assert!(command.contains("zip -r -7 '/tmp/a b.zip'"));
        assert!(command.contains("-x '*.gz'"));
        assert!(command.contains("'/var/log'"));
    }
}
