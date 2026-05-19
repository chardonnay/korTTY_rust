use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TerminalRecordingScope {
    ActiveSplit,
    WholeTab,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TerminalRecordingState {
    Recording,
    Paused,
    Stopped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TerminalRecordingExportFormat {
    Webm,
    Mkv,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalRecordingStartRequest {
    pub tab_id: String,
    pub split_id: Option<String>,
    pub connection_name: Option<String>,
    pub scope: TerminalRecordingScope,
    pub columns: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalRecordingStartResponse {
    pub session_id: String,
    pub file_path: String,
    pub state: TerminalRecordingState,
    pub started_at_millis: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalRecordingAppendSnapshotRequest {
    pub session_id: String,
    pub at_millis: Option<i64>,
    pub text: String,
    pub columns: u16,
    pub rows: u16,
    pub cursor_column: Option<u16>,
    pub cursor_row: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalRecordingAppendInputRequest {
    pub session_id: String,
    pub at_millis: Option<i64>,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalRecordingReplayEvent {
    pub event_type: String,
    pub at_millis: i64,
    pub text: Option<String>,
    pub columns: Option<u16>,
    pub rows: Option<u16>,
    pub cursor_column: Option<u16>,
    pub cursor_row: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalRecordingReplaySummary {
    pub id: String,
    pub name: String,
    pub file_path: String,
    pub size_bytes: u64,
    pub compressed: bool,
    pub started_at_millis: Option<i64>,
    pub ended_at_millis: Option<i64>,
    pub duration_millis: Option<i64>,
    pub event_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalRecordingReplayFile {
    pub summary: TerminalRecordingReplaySummary,
    pub events: Vec<TerminalRecordingReplayEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalRecordingVideoExportOptions {
    pub replay_path: String,
    pub target_path: String,
    pub format: TerminalRecordingExportFormat,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub frames_per_second: Option<u32>,
    pub speed: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalRecordingToolAvailability {
    pub available: bool,
    pub path: Option<String>,
}
