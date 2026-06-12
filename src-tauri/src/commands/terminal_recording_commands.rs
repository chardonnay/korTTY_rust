use crate::model::settings::GlobalSettings;
use crate::model::terminal_recording::{
    TerminalRecordingAppendInputRequest, TerminalRecordingAppendSnapshotRequest,
    TerminalRecordingExportProgressEvent, TerminalRecordingReplayFile,
    TerminalRecordingReplayFrames, TerminalRecordingReplaySummary, TerminalRecordingStartRequest,
    TerminalRecordingStartResponse, TerminalRecordingToolAvailability,
    TerminalRecordingVideoExportOptions,
};
use crate::persistence::xml_repository;
use crate::terminal_recording::{
    self, ExportProgressListener, TerminalRecordingStateListener, TerminalRecordingStore,
};
use tauri::{AppHandle, Emitter, State};

fn load_global_settings() -> Result<GlobalSettings, String> {
    let mut settings: GlobalSettings = xml_repository::load_json("global-settings.json")
        .map_err(|error| error.to_string())?
        .unwrap_or_default();
    settings.normalize();
    Ok(settings)
}

#[tauri::command]
pub fn start_terminal_recording(
    app: AppHandle,
    store: State<'_, TerminalRecordingStore>,
    request: TerminalRecordingStartRequest,
) -> Result<TerminalRecordingStartResponse, String> {
    let settings = load_global_settings()?;
    let listener: TerminalRecordingStateListener = Box::new(move |event| {
        let _ = app.emit("kortty-recording-state", event.clone());
    });
    store
        .start(request, &settings, Some(listener))
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn append_terminal_recording_snapshot(
    store: State<'_, TerminalRecordingStore>,
    request: TerminalRecordingAppendSnapshotRequest,
) -> Result<(), String> {
    store
        .append_snapshot(request)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn append_terminal_recording_input(
    store: State<'_, TerminalRecordingStore>,
    request: TerminalRecordingAppendInputRequest,
) -> Result<(), String> {
    store
        .append_input(request)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn pause_terminal_recording(
    store: State<'_, TerminalRecordingStore>,
    session_id: String,
) -> Result<(), String> {
    store.pause(&session_id).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn resume_terminal_recording(
    store: State<'_, TerminalRecordingStore>,
    session_id: String,
) -> Result<(), String> {
    store.resume(&session_id).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn stop_terminal_recording(
    store: State<'_, TerminalRecordingStore>,
    session_id: String,
) -> Result<TerminalRecordingReplaySummary, String> {
    store.stop(&session_id).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn list_terminal_recordings(
    directory: Option<String>,
) -> Result<Vec<TerminalRecordingReplaySummary>, String> {
    terminal_recording::list_replays(directory.as_deref()).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn load_terminal_recording(path: String) -> Result<TerminalRecordingReplayFile, String> {
    terminal_recording::load_replay(&path).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn load_terminal_recording_frames(
    path: String,
) -> Result<TerminalRecordingReplayFrames, String> {
    terminal_recording::load_replay_frames(&path).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn delete_terminal_recording(path: String) -> Result<(), String> {
    terminal_recording::delete_replay(&path).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn rename_terminal_recording(
    path: String,
    name: String,
) -> Result<TerminalRecordingReplaySummary, String> {
    terminal_recording::rename_replay(&path, &name).map_err(|error| error.to_string())
}

/// Checks ffmpeg by running `ffmpeg -version` (3s timeout). Uses the explicit
/// `configured_path` when given, otherwise the configured settings value.
#[tauri::command]
pub async fn check_terminal_recording_ffmpeg(
    configured_path: Option<String>,
) -> Result<TerminalRecordingToolAvailability, String> {
    let configured = match configured_path
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        Some(value) => Some(value),
        None => load_global_settings()?.terminal_recording_ffmpeg_path,
    };
    tauri::async_runtime::spawn_blocking(move || {
        terminal_recording::ffmpeg_availability(configured.as_deref())
    })
    .await
    .map_err(|error| error.to_string())
}

/// Renders and encodes a replay to webm/mkv, emitting
/// "kortty-recording-export-progress" {exportId, phase, fraction} events.
#[tauri::command]
pub async fn export_terminal_recording_video(
    app: AppHandle,
    export_id: String,
    options: TerminalRecordingVideoExportOptions,
) -> Result<(), String> {
    let settings = load_global_settings()?;
    let ffmpeg_path = settings.terminal_recording_ffmpeg_path.clone();
    let listener: ExportProgressListener = Box::new(move |phase, fraction| {
        let _ = app.emit(
            "kortty-recording-export-progress",
            TerminalRecordingExportProgressEvent {
                export_id: export_id.clone(),
                phase,
                fraction,
            },
        );
    });
    tauri::async_runtime::spawn_blocking(move || {
        terminal_recording::export_video(options, ffmpeg_path.as_deref(), Some(listener))
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())
}
