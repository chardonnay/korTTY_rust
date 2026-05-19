use crate::model::terminal_recording::{
    TerminalRecordingAppendInputRequest, TerminalRecordingAppendSnapshotRequest,
    TerminalRecordingReplayFile, TerminalRecordingReplaySummary, TerminalRecordingStartRequest,
    TerminalRecordingStartResponse, TerminalRecordingToolAvailability,
    TerminalRecordingVideoExportOptions,
};
use crate::terminal_recording::{self, TerminalRecordingStore};
use tauri::State;

#[tauri::command]
pub fn start_terminal_recording(
    store: State<'_, TerminalRecordingStore>,
    request: TerminalRecordingStartRequest,
) -> Result<TerminalRecordingStartResponse, String> {
    store.start(request).map_err(|error| error.to_string())
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

#[tauri::command]
pub fn check_terminal_recording_ffmpeg() -> TerminalRecordingToolAvailability {
    terminal_recording::ffmpeg_availability()
}

#[tauri::command]
pub fn export_terminal_recording_video(
    options: TerminalRecordingVideoExportOptions,
) -> Result<(), String> {
    terminal_recording::export_video(options).map_err(|error| error.to_string())
}
