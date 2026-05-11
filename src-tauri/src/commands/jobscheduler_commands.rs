use crate::jobscheduler::{probe_host_key, HostKeyProbeResult, JobSchedulerManager};
use crate::model::jobscheduler::{
    JobSchedulerStatusSummary, JournalEntry, PinnedHostKey, ScheduledJob, SudoCredential,
};
use crate::security::vault::Vault;
use tauri::{AppHandle, State};

#[tauri::command]
pub async fn get_scheduled_jobs(
    scheduler: State<'_, JobSchedulerManager>,
) -> Result<Vec<ScheduledJob>, String> {
    scheduler.jobs().map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn save_scheduled_job(
    scheduler: State<'_, JobSchedulerManager>,
    job: ScheduledJob,
) -> Result<ScheduledJob, String> {
    scheduler.save_job(job).map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn delete_scheduled_job(
    scheduler: State<'_, JobSchedulerManager>,
    id: String,
) -> Result<(), String> {
    scheduler.delete_job(&id).map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn run_scheduled_job_now(
    app: AppHandle,
    scheduler: State<'_, JobSchedulerManager>,
    vault: State<'_, Vault>,
    job_id: String,
) -> Result<String, String> {
    scheduler
        .run_job_now(app, &job_id, Some(vault))
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn cancel_scheduled_job_run(
    scheduler: State<'_, JobSchedulerManager>,
    run_id: String,
) -> Result<(), String> {
    scheduler
        .cancel_run(&run_id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn get_job_scheduler_status(
    scheduler: State<'_, JobSchedulerManager>,
) -> Result<JobSchedulerStatusSummary, String> {
    scheduler
        .status_summary()
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn get_job_journal(
    scheduler: State<'_, JobSchedulerManager>,
    job_id: Option<String>,
) -> Result<Vec<JournalEntry>, String> {
    scheduler
        .journal(job_id.as_deref())
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn clear_job_journal(
    scheduler: State<'_, JobSchedulerManager>,
    job_id: Option<String>,
) -> Result<(), String> {
    scheduler
        .clear_journal(job_id.as_deref())
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn probe_job_host_key(host: String, port: u16) -> Result<HostKeyProbeResult, String> {
    probe_host_key(&host, port)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn pin_job_host_key(
    scheduler: State<'_, JobSchedulerManager>,
    probe: HostKeyProbeResult,
    source: Option<String>,
) -> Result<PinnedHostKey, String> {
    scheduler
        .pin_host_key(probe, source)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn get_pinned_host_keys(
    scheduler: State<'_, JobSchedulerManager>,
) -> Result<Vec<PinnedHostKey>, String> {
    scheduler
        .pinned_host_keys()
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn delete_pinned_host_key(
    scheduler: State<'_, JobSchedulerManager>,
    id: String,
) -> Result<(), String> {
    scheduler
        .delete_pinned_host_key(&id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn encrypt_job_secret(
    vault: State<'_, Vault>,
    plaintext: String,
) -> Result<String, String> {
    vault
        .encrypt_value(&plaintext)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn save_job_sudo_credential(
    scheduler: State<'_, JobSchedulerManager>,
    credential: SudoCredential,
) -> Result<SudoCredential, String> {
    scheduler
        .save_sudo_credential(credential)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn delete_job_sudo_credential(
    scheduler: State<'_, JobSchedulerManager>,
    id: String,
) -> Result<(), String> {
    scheduler
        .delete_sudo_credential(&id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn drain_job_scheduler_on_shutdown(
    scheduler: State<'_, JobSchedulerManager>,
) -> Result<usize, String> {
    scheduler
        .drain_shutdown()
        .map_err(|error| error.to_string())
}
