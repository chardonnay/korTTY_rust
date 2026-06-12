//! Tauri commands for the update checker (Java `UpdateCheckService` facade).

use crate::update::service::{self, UpdateCheckService};
use crate::update::{download_dir, downloader, UpdateAsset, UpdateCheckResult, UpdateCheckRunType};
use serde_json::json;
use tauri::{AppHandle, Emitter, State};

/// Emit a download-progress event at most every 128 KiB to keep event
/// traffic reasonable for large installers.
const PROGRESS_STEP_BYTES: u64 = 128 * 1024;

/// Runs a manual update check; suppression rules (ignored/snoozed versions)
/// do not apply to manual checks.
#[tauri::command]
pub async fn check_for_updates_manually(app: AppHandle) -> Result<UpdateCheckResult, String> {
    Ok(service::check_for_update(&app, UpdateCheckRunType::Manual).await)
}

/// Downloads a release asset into the user's Downloads directory with SHA-256
/// verification and progress events (`update://download-progress` with
/// `{ bytesDone, bytesTotal }`). Returns the absolute path of the saved file.
#[tauri::command]
pub async fn download_update_asset(app: AppHandle, asset: UpdateAsset) -> Result<String, String> {
    let directory = download_dir::resolve_default_downloads_directory();
    let progress_app = app.clone();
    let mut last_emitted: u64 = 0;
    let path = downloader::download(&asset, &directory, move |bytes_done, bytes_total| {
        if bytes_done.saturating_sub(last_emitted) >= PROGRESS_STEP_BYTES {
            last_emitted = bytes_done;
            let _ = progress_app.emit(
                service::EVENT_DOWNLOAD_PROGRESS,
                json!({ "bytesDone": bytes_done, "bytesTotal": bytes_total }),
            );
        }
    })
    .await
    .map_err(|error| error.to_string())?;

    // Final event so listeners always see the completed state.
    let bytes_done = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
    let _ = app.emit(
        service::EVENT_DOWNLOAD_PROGRESS,
        json!({ "bytesDone": bytes_done, "bytesTotal": asset.size }),
    );
    Ok(path.to_string_lossy().into_owned())
}

/// Snoozes automatic prompts for `version` until the next local calendar day.
#[tauri::command]
pub async fn snooze_update_version(app: AppHandle, version: String) -> Result<(), String> {
    service::snooze_until_tomorrow(&app, &version);
    Ok(())
}

/// Permanently ignores automatic prompts for `version`.
#[tauri::command]
pub async fn ignore_update_version(app: AppHandle, version: String) -> Result<(), String> {
    service::ignore_version(&app, &version);
    Ok(())
}

/// Restarts the background check service, e.g. after the update settings
/// changed. A disabled service simply stays stopped.
#[tauri::command]
pub async fn restart_update_check_service(
    app: AppHandle,
    service_state: State<'_, UpdateCheckService>,
) -> Result<(), String> {
    service_state.restart(app).await;
    Ok(())
}

/// Returns the directory update assets are downloaded into.
#[tauri::command]
pub async fn get_update_download_directory() -> Result<String, String> {
    Ok(download_dir::resolve_default_downloads_directory()
        .to_string_lossy()
        .into_owned())
}
