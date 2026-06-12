use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TerminalRecordingScope {
    ActiveSplit,
    WholeTab,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum TerminalRecordingFormat {
    #[default]
    KorttyReplay,
    Webm,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TerminalRecordingState {
    Idle,
    Recording,
    Paused,
    AutoPaused,
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
    #[serde(default)]
    pub widget: Option<String>,
    #[serde(default)]
    pub pixel_width: Option<u32>,
    #[serde(default)]
    pub pixel_height: Option<u32>,
    #[serde(default)]
    pub style_runs: Vec<TerminalRecordingStyleRun>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalRecordingAppendInputRequest {
    pub session_id: String,
    pub at_millis: Option<i64>,
    pub text: String,
}

/// One styled text run of a recorded screen snapshot (Java: TerminalRecordingStyleRun).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TerminalRecordingStyleRun {
    #[serde(default)]
    pub row: u32,
    #[serde(default)]
    pub column: u32,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreground: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
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
    #[serde(default)]
    pub widget: Option<String>,
    #[serde(default)]
    pub pixel_width: Option<u32>,
    #[serde(default)]
    pub pixel_height: Option<u32>,
    #[serde(default)]
    pub style_runs: Vec<TerminalRecordingStyleRun>,
}

/// One replay frame with its display duration (Java: TerminalRecordingReplayFrame).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalRecordingReplayFrame {
    pub content: String,
    #[serde(default)]
    pub columns: u32,
    #[serde(default)]
    pub rows: u32,
    #[serde(default)]
    pub pixel_width: u32,
    #[serde(default)]
    pub pixel_height: u32,
    #[serde(default)]
    pub style_runs: Vec<TerminalRecordingStyleRun>,
    pub duration_seconds: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalRecordingReplayFrames {
    pub frames: Vec<TerminalRecordingReplayFrame>,
    pub total_duration_seconds: f64,
}

/// Payload emitted to the UI on every recording state change ("kortty-recording-state").
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalRecordingStateEvent {
    pub session_id: String,
    pub tab_id: String,
    pub split_id: Option<String>,
    pub state: TerminalRecordingState,
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

fn default_include_color() -> bool {
    true
}

/// Video export request (Java: TerminalRecordingExportOptions + TerminalRecordingTimeRange).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalRecordingVideoExportOptions {
    pub replay_path: String,
    pub target_path: String,
    pub format: TerminalRecordingExportFormat,
    #[serde(default)]
    pub start_seconds: Option<f64>,
    #[serde(default)]
    pub end_seconds: Option<f64>,
    #[serde(default = "default_include_color")]
    pub include_color: bool,
}

/// Export pipeline phase (Java: TerminalRecordingService.ExportPhase).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum TerminalRecordingExportPhase {
    Preparing,
    Rendering,
    Encoding,
    Finalizing,
}

/// Payload emitted to the UI as "kortty-recording-export-progress".
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalRecordingExportProgressEvent {
    pub export_id: String,
    pub phase: TerminalRecordingExportPhase,
    pub fraction: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalRecordingToolAvailability {
    pub available: bool,
    pub resolved_path: Option<String>,
}
