use crate::model::settings::GlobalSettings;
use crate::model::terminal_recording::{
    TerminalRecordingAppendInputRequest, TerminalRecordingAppendSnapshotRequest,
    TerminalRecordingExportFormat, TerminalRecordingExportPhase, TerminalRecordingReplayEvent,
    TerminalRecordingReplayFile, TerminalRecordingReplayFrame, TerminalRecordingReplayFrames,
    TerminalRecordingReplaySummary, TerminalRecordingScope, TerminalRecordingStartRequest,
    TerminalRecordingStartResponse, TerminalRecordingState, TerminalRecordingStateEvent,
    TerminalRecordingStyleRun, TerminalRecordingToolAvailability,
    TerminalRecordingVideoExportOptions,
};
use crate::persistence::xml_repository;
use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use anyhow::{anyhow, Context, Result};
use chrono::SecondsFormat;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

const REPLAY_FORMAT_VERSION: u32 = 1;
const COMPRESSED_REPLAY_EXTENSION: &str = ".korttyrec.jsonl.gz";
const MIN_FRAME_DURATION_SECONDS: f64 = 0.001;
const FALLBACK_LAST_FRAME_DURATION_SECONDS: f64 = 1.0;

// Video export constants (Java: TerminalRecordingService).
const FRAME_PADDING_X: u32 = 32;
const FRAME_PADDING_Y: u32 = 24;
const EXPORT_FONT_SIZE: f32 = 20.0;
const MAX_RENDER_THREADS: usize = 4;
const MAX_PROCESS_OUTPUT_BYTES: usize = 64 * 1024;
const FFMPEG_EXPORT_TIMEOUT: Duration = Duration::from_secs(300);
const FFMPEG_AVAILABILITY_TIMEOUT: Duration = Duration::from_secs(3);
const RENDER_PROGRESS_WEIGHT: f64 = 0.70;
const ENCODE_PROGRESS_WEIGHT: f64 = 0.29;
const EXPORT_FOREGROUND: Rgb = [220, 235, 220];
const EXPORT_BACKGROUND: Rgb = [0, 0, 0];

// JetBrains Mono (SIL Open Font License 1.1, see resources/fonts/JetBrainsMono-OFL.txt).
static EXPORT_FONT_REGULAR: &[u8] = include_bytes!("../resources/fonts/JetBrainsMono-Regular.ttf");
static EXPORT_FONT_BOLD: &[u8] = include_bytes!("../resources/fonts/JetBrainsMono-Bold.ttf");

type Rgb = [u8; 3];

/// Listener invoked with every export progress update (used to emit
/// "kortty-recording-export-progress"). Called from worker threads.
pub type ExportProgressListener = Box<dyn Fn(TerminalRecordingExportPhase, f64) + Send + Sync>;

/// Listener invoked on every recording state change (used to emit "kortty-recording-state").
pub type TerminalRecordingStateListener =
    Box<dyn Fn(&TerminalRecordingStateEvent) + Send + Sync + 'static>;

fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn scope_name(scope: &TerminalRecordingScope) -> &'static str {
    match scope {
        TerminalRecordingScope::ActiveSplit => "ACTIVE_SPLIT",
        TerminalRecordingScope::WholeTab => "WHOLE_TAB",
    }
}

/// Screen snapshot contents as recorded for one widget (Java: TerminalRecordingScreenSnapshot).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ScreenSnapshot {
    pub content: String,
    pub columns: u32,
    pub rows: u32,
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub style_runs: Vec<TerminalRecordingStyleRun>,
}

impl ScreenSnapshot {
    pub fn plain(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            ..Self::default()
        }
    }

    fn to_frame(&self, duration_seconds: f64) -> TerminalRecordingReplayFrame {
        TerminalRecordingReplayFrame {
            content: self.content.clone(),
            columns: self.columns,
            rows: self.rows,
            pixel_width: self.pixel_width,
            pixel_height: self.pixel_height,
            style_runs: self.style_runs.clone(),
            duration_seconds,
        }
    }

    /// Builds the JSON payload of a "screen" event (Java field names/optionality).
    fn event_payload(&self, widget: &str) -> Map<String, Value> {
        let mut payload = Map::new();
        payload.insert("widget".into(), json!(widget));
        payload.insert("content".into(), json!(self.content));
        if self.columns > 0 {
            payload.insert("columns".into(), json!(self.columns));
        }
        if self.rows > 0 {
            payload.insert("rows".into(), json!(self.rows));
        }
        if self.pixel_width > 0 {
            payload.insert("pixelWidth".into(), json!(self.pixel_width));
        }
        if self.pixel_height > 0 {
            payload.insert("pixelHeight".into(), json!(self.pixel_height));
        }
        if !self.style_runs.is_empty() {
            payload.insert(
                "styleRuns".into(),
                serde_json::to_value(&self.style_runs).unwrap_or(Value::Null),
            );
        }
        payload
    }
}

pub struct TerminalRecordingSession {
    session_id: String,
    tab_id: String,
    split_id: Option<String>,
    file_path: PathBuf,
    writer: Option<GzEncoder<File>>,
    state: TerminalRecordingState,
    auto_pause_enabled: bool,
    idle_pause_millis: i64,
    last_activity_millis: i64,
    last_screen_by_widget: HashMap<String, String>,
    segment_count: u32,
    state_listener: Option<TerminalRecordingStateListener>,
    idle_task: Option<tauri::async_runtime::JoinHandle<()>>,
}

impl TerminalRecordingSession {
    #[allow(clippy::too_many_arguments)]
    fn create(
        session_id: String,
        tab_id: String,
        split_id: Option<String>,
        file_path: PathBuf,
        connection_name: &str,
        auto_pause_enabled: bool,
        idle_pause_seconds: u32,
        started_at_millis: i64,
        state_listener: Option<TerminalRecordingStateListener>,
    ) -> Result<Self> {
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = File::create(&file_path)
            .with_context(|| format!("Failed to create {}", file_path.display()))?;
        let writer = GzEncoder::new(file, Compression::default());
        let safe_connection = if connection_name.trim().is_empty() {
            "Terminal"
        } else {
            connection_name
        };
        let mut session = Self {
            session_id,
            tab_id,
            split_id,
            file_path,
            writer: Some(writer),
            state: TerminalRecordingState::Idle,
            auto_pause_enabled,
            idle_pause_millis: i64::from(idle_pause_seconds.max(1)) * 1000,
            last_activity_millis: started_at_millis,
            last_screen_by_widget: HashMap::new(),
            segment_count: 0,
            state_listener,
            idle_task: None,
        };
        let mut event = session.event("session_created");
        event.insert("connection".into(), json!(safe_connection));
        event.insert("formatVersion".into(), json!(REPLAY_FORMAT_VERSION));
        session.write_event(Value::Object(event))?;
        Ok(session)
    }

    fn is_active(&self) -> bool {
        matches!(
            self.state,
            TerminalRecordingState::Recording
                | TerminalRecordingState::AutoPaused
                | TerminalRecordingState::Paused
        )
    }

    fn is_recording_or_auto_paused(&self) -> bool {
        matches!(
            self.state,
            TerminalRecordingState::Recording | TerminalRecordingState::AutoPaused
        )
    }

    fn is_closed(&self) -> bool {
        self.writer.is_none()
    }

    fn ensure_open(&self) -> Result<()> {
        if self.is_closed() {
            return Err(anyhow!("Terminal recording session is already closed"));
        }
        Ok(())
    }

    fn start(&mut self, scope: &TerminalRecordingScope) -> Result<()> {
        self.ensure_open()?;
        if self.is_active() {
            return Ok(());
        }
        self.segment_count += 1;
        self.last_activity_millis = now_millis();
        self.set_state(TerminalRecordingState::Recording);
        let mut event = self.event("recording_start");
        event.insert("scope".into(), json!(scope_name(scope)));
        event.insert("segment".into(), json!(self.segment_count));
        self.write_event(Value::Object(event))
    }

    fn stop(&mut self) -> Result<()> {
        self.ensure_open()?;
        if !self.is_active() {
            return Ok(());
        }
        let mut event = self.event("recording_stop");
        event.insert("segment".into(), json!(self.segment_count));
        self.write_event(Value::Object(event))?;
        self.set_state(TerminalRecordingState::Stopped);
        Ok(())
    }

    fn record_screen_snapshot(&mut self, widget_id: &str, snapshot: &ScreenSnapshot) -> Result<()> {
        if !self.is_recording_or_auto_paused() {
            return Ok(());
        }
        let safe_widget = if widget_id.trim().is_empty() {
            "terminal"
        } else {
            widget_id
        };
        let payload = snapshot.event_payload(safe_widget);
        let fingerprint = Value::Object(payload.clone()).to_string();
        if self.last_screen_by_widget.get(safe_widget) == Some(&fingerprint) {
            return Ok(());
        }
        self.record_activity("screen", now_millis())?;
        self.last_screen_by_widget
            .insert(safe_widget.to_string(), fingerprint);
        let mut event = self.event("screen");
        event.extend(payload);
        self.write_event(Value::Object(event))
    }

    fn record_user_input_activity(&mut self) -> Result<()> {
        if !self.is_recording_or_auto_paused() {
            return Ok(());
        }
        self.record_activity("user_input", now_millis())?;
        let event = self.event("user_input_activity");
        self.write_event(Value::Object(event))
    }

    /// Writes an auto_pause event when the recording was idle for at least the
    /// configured idle pause duration (Java: TerminalRecordingSession#checkIdle).
    fn check_idle(&mut self, now_millis: i64) -> Result<()> {
        if !self.auto_pause_enabled || self.state != TerminalRecordingState::Recording {
            return Ok(());
        }
        let idle_millis = now_millis - self.last_activity_millis;
        if idle_millis >= self.idle_pause_millis {
            let mut event = self.event("auto_pause");
            event.insert("idleMillis".into(), json!(idle_millis));
            self.write_event(Value::Object(event))?;
            self.set_state(TerminalRecordingState::AutoPaused);
        }
        Ok(())
    }

    /// Java semantics: an auto-paused session resumes on any activity; the
    /// activity timestamp is always refreshed.
    fn record_activity(&mut self, source: &str, now_millis: i64) -> Result<()> {
        if self.state == TerminalRecordingState::AutoPaused {
            let mut event = self.event("auto_resume");
            event.insert("source".into(), json!(source));
            self.write_event(Value::Object(event))?;
            self.set_state(TerminalRecordingState::Recording);
        }
        self.last_activity_millis = now_millis;
        Ok(())
    }

    /// Manual pause requested by the user. Stops the replay clock via an
    /// auto_pause marker but does not auto-resume on activity.
    fn pause_manual(&mut self) -> Result<()> {
        self.ensure_open()?;
        if !self.is_recording_or_auto_paused() {
            return Ok(());
        }
        if self.state == TerminalRecordingState::Recording {
            let mut event = self.event("auto_pause");
            event.insert("idleMillis".into(), json!(0));
            self.write_event(Value::Object(event))?;
        }
        self.set_state(TerminalRecordingState::Paused);
        Ok(())
    }

    fn resume_manual(&mut self) -> Result<()> {
        self.ensure_open()?;
        if !matches!(
            self.state,
            TerminalRecordingState::Paused | TerminalRecordingState::AutoPaused
        ) {
            return Ok(());
        }
        let mut event = self.event("auto_resume");
        event.insert("source".into(), json!("manual"));
        self.write_event(Value::Object(event))?;
        self.last_activity_millis = now_millis();
        self.set_state(TerminalRecordingState::Recording);
        Ok(())
    }

    fn close(&mut self) -> Result<()> {
        if let Some(task) = self.idle_task.take() {
            task.abort();
        }
        if self.is_closed() {
            return Ok(());
        }
        if self.is_active() {
            self.stop()?;
        }
        let event = self.event("session_closed");
        self.write_event(Value::Object(event))?;
        if let Some(writer) = self.writer.take() {
            writer.finish()?;
        }
        Ok(())
    }

    fn set_state(&mut self, new_state: TerminalRecordingState) {
        if self.state == new_state {
            return;
        }
        self.state = new_state.clone();
        if let Some(listener) = &self.state_listener {
            listener(&TerminalRecordingStateEvent {
                session_id: self.session_id.clone(),
                tab_id: self.tab_id.clone(),
                split_id: self.split_id.clone(),
                state: new_state,
            });
        }
    }

    fn event(&self, event_type: &str) -> Map<String, Value> {
        let mut event = Map::new();
        event.insert("type".into(), json!(event_type));
        event.insert("at".into(), json!(now_iso()));
        event
    }

    fn write_event(&mut self, event: Value) -> Result<()> {
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| anyhow!("Terminal recording session is already closed"))?;
        serde_json::to_writer(&mut *writer, &event)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
        Ok(())
    }
}

impl Drop for TerminalRecordingSession {
    fn drop(&mut self) {
        if let Some(task) = self.idle_task.take() {
            task.abort();
        }
        if !self.is_closed() {
            if let Err(error) = self.close() {
                tracing::warn!(error = %error, "failed to close terminal recording session");
            }
        }
    }
}

pub struct TerminalRecordingStore {
    sessions: Mutex<HashMap<String, Arc<Mutex<TerminalRecordingSession>>>>,
    /// Maps an SSH terminal-session id (a split's or single terminal's id) to
    /// the recording session id that should receive its input activity. For a
    /// WHOLE_TAB recording every split of the tab is registered here so that
    /// input from any split reaches the single tab recording session — the
    /// recording session itself only stores the tab id, not the per-split ids.
    terminal_to_session: Mutex<HashMap<String, String>>,
}

impl TerminalRecordingStore {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            terminal_to_session: Mutex::new(HashMap::new()),
        }
    }

    pub fn start(
        &self,
        request: TerminalRecordingStartRequest,
        settings: &GlobalSettings,
        state_listener: Option<TerminalRecordingStateListener>,
    ) -> Result<TerminalRecordingStartResponse> {
        let session_id = uuid::Uuid::new_v4().to_string();
        let started_at_millis = now_millis();
        let directory =
            resolve_recording_directory(settings.terminal_recording_directory.as_deref())?;
        fs::create_dir_all(&directory)?;
        let connection_name = request
            .connection_name
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "Terminal".to_string());
        let terminal_session_part = request
            .split_id
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| request.tab_id.clone());
        let file_path = next_replay_file(&directory, &connection_name, &terminal_session_part);
        let mut session = TerminalRecordingSession::create(
            session_id.clone(),
            request.tab_id.clone(),
            request.split_id.clone(),
            file_path.clone(),
            &connection_name,
            settings.terminal_recording_idle_auto_pause,
            settings.terminal_recording_idle_pause_seconds,
            started_at_millis,
            state_listener,
        )?;
        session.start(&request.scope)?;
        let session_arc = Arc::new(Mutex::new(session));
        let idle_task = spawn_idle_monitor(Arc::downgrade(&session_arc));
        if let Ok(mut guard) = session_arc.lock() {
            guard.idle_task = Some(idle_task);
        }
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|error| anyhow!(error.to_string()))?;
        sessions.insert(session_id.clone(), session_arc);
        // Register the primary terminal id (the active split for ACTIVE_SPLIT,
        // or the tab itself when unsplit) so its input routes immediately, even
        // before the first screen snapshot arrives. Additional splits of a
        // WHOLE_TAB recording are registered lazily from their snapshots.
        let primary_terminal_id = request
            .split_id
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| request.tab_id.clone());
        if let Ok(mut mapping) = self.terminal_to_session.lock() {
            mapping.insert(primary_terminal_id, session_id.clone());
        }
        Ok(TerminalRecordingStartResponse {
            session_id,
            file_path: file_path.to_string_lossy().to_string(),
            state: TerminalRecordingState::Recording,
            started_at_millis,
        })
    }

    pub fn append_snapshot(&self, request: TerminalRecordingAppendSnapshotRequest) -> Result<()> {
        let session = self.session(&request.session_id)?;
        let mut guard = session.lock().map_err(|error| anyhow!(error.to_string()))?;
        let widget = request
            .widget
            .clone()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| guard.split_id.clone())
            .unwrap_or_else(|| guard.tab_id.clone());
        // The widget id is the SSH terminal-session id of the split that
        // produced this snapshot. Registering it lets input from any split of a
        // WHOLE_TAB recording reach this recording session.
        if let Ok(mut mapping) = self.terminal_to_session.lock() {
            mapping.insert(widget.clone(), request.session_id.clone());
        }
        let snapshot = ScreenSnapshot {
            content: request.text,
            columns: u32::from(request.columns),
            rows: u32::from(request.rows),
            pixel_width: request.pixel_width.unwrap_or(0),
            pixel_height: request.pixel_height.unwrap_or(0),
            style_runs: request.style_runs,
        };
        guard.record_screen_snapshot(&widget, &snapshot)
    }

    /// Records pure input activity. The input text is intentionally never
    /// persisted (privacy, Java parity).
    pub fn append_input(&self, request: TerminalRecordingAppendInputRequest) -> Result<()> {
        let session = self.session(&request.session_id)?;
        let mut guard = session.lock().map_err(|error| anyhow!(error.to_string()))?;
        guard.record_user_input_activity()
    }

    /// Routes input activity from an SSH terminal session (tab or split id) to
    /// the recording attached to that terminal. Never fails; errors are logged.
    ///
    /// For a WHOLE_TAB recording the incoming `terminal_session_id` is a split's
    /// SSH session id, which differs from the recording's stored tab id; the
    /// split → recording-session mapping (populated on start and on each screen
    /// snapshot) resolves it to the correct tab recording. A direct
    /// split_id/tab_id comparison is kept as a fallback for sessions whose
    /// mapping has not been established yet.
    pub fn record_input_activity_for_terminal(&self, terminal_session_id: &str) {
        // Resolve the recording session via the terminal → session mapping. This
        // is the WHOLE_TAB path: input from any split of the tab routes here.
        let mapped_session = self
            .terminal_to_session
            .lock()
            .ok()
            .and_then(|mapping| mapping.get(terminal_session_id).cloned())
            .and_then(|session_id| {
                self.sessions
                    .lock()
                    .ok()
                    .and_then(|map| map.get(&session_id).cloned())
            });
        if let Some(session) = mapped_session {
            if let Ok(mut guard) = session.lock() {
                if let Err(error) = guard.record_user_input_activity() {
                    tracing::warn!(error = %error, "failed to record terminal recording input activity");
                }
            }
            return;
        }

        // Fallback: direct match against the recording's own tab/split ids for
        // sessions that have not yet been registered in the mapping.
        let sessions: Vec<Arc<Mutex<TerminalRecordingSession>>> = match self.sessions.lock() {
            Ok(map) => map.values().cloned().collect(),
            Err(_) => return,
        };
        for session in sessions {
            let Ok(mut guard) = session.lock() else {
                continue;
            };
            let matches = guard.split_id.as_deref() == Some(terminal_session_id)
                || (guard.split_id.is_none() && guard.tab_id == terminal_session_id);
            if matches {
                if let Err(error) = guard.record_user_input_activity() {
                    tracing::warn!(error = %error, "failed to record terminal recording input activity");
                }
            }
        }
    }

    pub fn pause(&self, session_id: &str) -> Result<()> {
        let session = self.session(session_id)?;
        let mut guard = session.lock().map_err(|error| anyhow!(error.to_string()))?;
        guard.pause_manual()
    }

    pub fn resume(&self, session_id: &str) -> Result<()> {
        let session = self.session(session_id)?;
        let mut guard = session.lock().map_err(|error| anyhow!(error.to_string()))?;
        guard.resume_manual()
    }

    pub fn stop(&self, session_id: &str) -> Result<TerminalRecordingReplaySummary> {
        let session = {
            let mut sessions = self
                .sessions
                .lock()
                .map_err(|error| anyhow!(error.to_string()))?;
            sessions
                .remove(session_id)
                .ok_or_else(|| anyhow!("Recording session not found"))?
        };
        // Drop every terminal → session mapping entry pointing at this recording
        // so a later split reusing the same id is not routed to a stopped session.
        if let Ok(mut mapping) = self.terminal_to_session.lock() {
            mapping.retain(|_, mapped| mapped != session_id);
        }
        let file_path = {
            let mut guard = session.lock().map_err(|error| anyhow!(error.to_string()))?;
            guard.close()?;
            guard.file_path.clone()
        };
        summarize_replay(&file_path)
    }

    fn session(&self, session_id: &str) -> Result<Arc<Mutex<TerminalRecordingSession>>> {
        let sessions = self
            .sessions
            .lock()
            .map_err(|error| anyhow!(error.to_string()))?;
        sessions
            .get(session_id)
            .cloned()
            .ok_or_else(|| anyhow!("Recording session not found"))
    }
}

impl Default for TerminalRecordingStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-session idle monitor; ticks every second and auto-pauses idle
/// recordings. Holds only a weak reference so a dropped session ends the task.
fn spawn_idle_monitor(
    session: Weak<Mutex<TerminalRecordingSession>>,
) -> tauri::async_runtime::JoinHandle<()> {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            let Some(session) = session.upgrade() else {
                break;
            };
            let Ok(mut guard) = session.lock() else {
                break;
            };
            if guard.is_closed() {
                break;
            }
            if let Err(error) = guard.check_idle(now_millis()) {
                tracing::warn!(error = %error, "terminal recording idle check failed");
            }
            // Guard is dropped here, before the next await point.
        }
    })
}

pub fn default_recording_dir() -> Result<PathBuf> {
    Ok(xml_repository::config_dir()?.join("recordings"))
}

pub fn resolve_recording_directory(configured: Option<&str>) -> Result<PathBuf> {
    if let Some(path) = configured.map(str::trim).filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    default_recording_dir()
}

/// Java: TerminalRecordingService#sanitizeFileName.
pub fn sanitize_file_name(value: &str) -> String {
    let normalized = value.trim();
    if normalized.is_empty() {
        return "terminal".into();
    }
    let mut sanitized = String::with_capacity(normalized.len());
    let mut last_was_underscore = false;
    for ch in normalized.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-') {
            sanitized.push(ch);
            last_was_underscore = false;
        } else if !last_was_underscore {
            sanitized.push('_');
            last_was_underscore = true;
        }
    }
    let trimmed = sanitized.trim_matches('_');
    if trimmed.is_empty() {
        "terminal".into()
    } else {
        trimmed.to_string()
    }
}

/// Java: TerminalRecordingService#nextReplayFile —
/// {sanitizedName}-{yyyyMMdd-HHmmss}-{sessionIdPart<=12}.korttyrec.jsonl.gz
/// with a collision counter suffix.
fn next_replay_file(directory: &Path, connection_name: &str, terminal_session_id: &str) -> PathBuf {
    let base_name = sanitize_file_name(connection_name);
    let session_part: String = sanitize_file_name(terminal_session_id)
        .chars()
        .take(12)
        .collect();
    let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let mut candidate = directory.join(format!(
        "{base_name}-{timestamp}-{session_part}{COMPRESSED_REPLAY_EXTENSION}"
    ));
    let mut counter = 2;
    while candidate.exists() {
        candidate = directory.join(format!(
            "{base_name}-{timestamp}-{session_part}-{counter}{COMPRESSED_REPLAY_EXTENSION}"
        ));
        counter += 1;
    }
    candidate
}

pub fn list_replays(directory: Option<&str>) -> Result<Vec<TerminalRecordingReplaySummary>> {
    let directory = if let Some(directory) = directory.filter(|value| !value.trim().is_empty()) {
        PathBuf::from(directory)
    } else {
        default_recording_dir()?
    };
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut summaries = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if is_replay_path(&path) {
            match summarize_replay(&path) {
                Ok(summary) => summaries.push(summary),
                Err(error) => {
                    tracing::warn!(path = %path.display(), error = %error, "skipping invalid terminal replay")
                }
            }
        }
    }
    summaries.sort_by(|left, right| {
        right
            .started_at_millis
            .cmp(&left.started_at_millis)
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(summaries)
}

pub fn load_replay(path: &str) -> Result<TerminalRecordingReplayFile> {
    let path = PathBuf::from(path);
    let summary = summarize_replay(&path)?;
    let events = read_replay_events(&path)?;
    Ok(TerminalRecordingReplayFile { summary, events })
}

/// Builds timed replay frames for the replay viewer (Java: loadReplayFrames).
pub fn load_replay_frames(path: &str) -> Result<TerminalRecordingReplayFrames> {
    let path = PathBuf::from(path);
    ensure_replay_path(&path)?;
    let frames = build_timed_replay_frames(&path)?;
    let total_duration_seconds = total_duration_seconds(&frames);
    Ok(TerminalRecordingReplayFrames {
        frames,
        total_duration_seconds,
    })
}

pub fn delete_replay(path: &str) -> Result<()> {
    let path = PathBuf::from(path);
    ensure_replay_path(&path)?;
    fs::remove_file(path)?;
    Ok(())
}

pub fn rename_replay(path: &str, name: &str) -> Result<TerminalRecordingReplaySummary> {
    let path = PathBuf::from(path);
    ensure_replay_path(&path)?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("Replay path has no parent directory"))?;
    let extension = replay_extension(&path).ok_or_else(|| anyhow!("Unsupported replay file"))?;
    let mut base_name = name.trim().to_string();
    if let Some(requested_extension) = replay_extension(Path::new(&base_name)) {
        base_name.truncate(base_name.len() - requested_extension.len());
    }
    if base_name.trim().is_empty() {
        return Err(anyhow!("Replay file name is empty"));
    }
    let target_name = format!("{}{}", sanitize_file_name(&base_name), extension);
    let target = parent.join(target_name);
    fs::rename(&path, &target)?;
    summarize_replay(&target)
}

/// Java: TerminalRecordingService#isFfmpegAvailable — runs `ffmpeg -version`
/// with a 3 second timeout instead of a plain PATH lookup.
pub fn ffmpeg_availability(configured_path: Option<&str>) -> TerminalRecordingToolAvailability {
    let executable = normalize_ffmpeg_executable(configured_path);
    let available = probe_ffmpeg_version(&executable);
    let resolved_path = if available {
        if Path::new(&executable).components().count() > 1 {
            Some(executable)
        } else {
            find_tool(&executable)
                .map(|path| path.to_string_lossy().to_string())
                .or(Some(executable))
        }
    } else {
        None
    };
    TerminalRecordingToolAvailability {
        available,
        resolved_path,
    }
}

fn probe_ffmpeg_version(executable: &str) -> bool {
    let mut child = match Command::new(executable)
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };
    let deadline = Instant::now() + FFMPEG_AVAILABILITY_TIMEOUT;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    false
}

/// Java: TerminalRecordingService#normalizeFfmpegExecutable.
fn normalize_ffmpeg_executable(configured_path: Option<&str>) -> String {
    configured_path
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| "ffmpeg".to_string())
}

/// Java: TerminalRecordingService#exportReplay. Blocking — callers run it on a
/// blocking thread (tauri::async_runtime::spawn_blocking).
pub fn export_video(
    options: TerminalRecordingVideoExportOptions,
    configured_ffmpeg_path: Option<&str>,
    progress: Option<ExportProgressListener>,
) -> Result<()> {
    let progress = progress.as_ref();
    let replay_path = PathBuf::from(&options.replay_path);
    ensure_replay_path(&replay_path)?;
    if !replay_path.is_file() {
        return Err(anyhow!(
            "Replay file does not exist: {}",
            replay_path.display()
        ));
    }
    let target = PathBuf::from(&options.target_path);
    validate_video_extension(&target, &options.format)?;
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let ffmpeg = normalize_ffmpeg_executable(configured_ffmpeg_path);
    let fonts = ExportFonts::load()?;

    report_progress(progress, TerminalRecordingExportPhase::Preparing, 0.0);
    let mut frames = slice_replay_frames(
        &build_timed_replay_frames(&replay_path)?,
        options.start_seconds.unwrap_or(0.0),
        options.end_seconds.unwrap_or(f64::INFINITY),
    )?;
    if frames.is_empty() {
        frames = vec![
            ScreenSnapshot::plain("No terminal screen frames were recorded.")
                .to_frame(FALLBACK_LAST_FRAME_DURATION_SECONDS),
        ];
    }
    let total_duration = total_duration_seconds(&frames);

    let temp_dir = std::env::temp_dir().join(format!(
        "kortty-recording-export-{}",
        uuid::Uuid::new_v4().simple()
    ));
    fs::create_dir_all(&temp_dir)?;
    let result = (|| -> Result<()> {
        let layout = resolve_render_layout(&frames, &fonts);
        let frame_files = render_frame_images(
            &temp_dir,
            &frames,
            &layout,
            options.include_color,
            &fonts,
            progress,
        )?;
        let concat_file = temp_dir.join("frames.txt");
        fs::write(
            &concat_file,
            build_concat_file_content(&frame_files, &frames),
        )?;
        run_ffmpeg_export(
            &ffmpeg,
            &concat_file,
            &target,
            &options.format,
            total_duration,
            progress,
        )?;
        if !target.is_file() {
            return Err(anyhow!("ffmpeg did not create a video file"));
        }
        report_progress(progress, TerminalRecordingExportPhase::Finalizing, 1.0);
        Ok(())
    })();
    cleanup_temp_dir_async(temp_dir);
    result
}

/// Best-effort temp cleanup on a daemon-style thread (Java: deleteRecursivelyAsync).
fn cleanup_temp_dir_async(temp_dir: PathBuf) {
    let _ = std::thread::Builder::new()
        .name("kortty-recording-export-cleanup".into())
        .spawn(move || {
            if let Err(error) = fs::remove_dir_all(&temp_dir) {
                tracing::debug!(
                    path = %temp_dir.display(),
                    error = %error,
                    "could not clean terminal recording export temp directory"
                );
            }
        });
}

fn report_progress(
    listener: Option<&ExportProgressListener>,
    phase: TerminalRecordingExportPhase,
    fraction: f64,
) {
    if let Some(listener) = listener {
        let safe_fraction = if fraction.is_finite() {
            fraction.clamp(0.0, 1.0)
        } else {
            0.0
        };
        listener(phase, safe_fraction);
    }
}

struct ExportFonts {
    regular: FontRef<'static>,
    bold: FontRef<'static>,
}

impl ExportFonts {
    fn load() -> Result<Self> {
        Ok(Self {
            regular: FontRef::try_from_slice(EXPORT_FONT_REGULAR)
                .map_err(|error| anyhow!("Failed to load embedded export font: {error}"))?,
            bold: FontRef::try_from_slice(EXPORT_FONT_BOLD)
                .map_err(|error| anyhow!("Failed to load embedded bold export font: {error}"))?,
        })
    }
}

/// Java: TerminalRecordingService.RenderLayout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RenderLayout {
    width: u32,
    height: u32,
    cell_width: u32,
    line_height: u32,
    ascent: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrameExtents {
    max_columns: u32,
    max_rows: u32,
    max_pixel_width: u32,
    max_pixel_height: u32,
}

fn collect_frame_extents(frames: &[TerminalRecordingReplayFrame]) -> FrameExtents {
    let mut extents = FrameExtents {
        max_columns: 1,
        max_rows: 1,
        max_pixel_width: 0,
        max_pixel_height: 0,
    };
    for frame in frames {
        extents.max_columns = extents.max_columns.max(frame.columns);
        extents.max_rows = extents.max_rows.max(frame.rows);
        extents.max_pixel_width = extents.max_pixel_width.max(frame.pixel_width);
        extents.max_pixel_height = extents.max_pixel_height.max(frame.pixel_height);
        let lines = split_screen_lines(&frame.content);
        extents.max_rows = extents.max_rows.max(lines.len() as u32);
        for line in &lines {
            extents.max_columns = extents.max_columns.max(line.chars().count() as u32);
        }
        for run in &frame.style_runs {
            extents.max_columns = extents
                .max_columns
                .max(run.column.saturating_add(run.text.chars().count() as u32));
            extents.max_rows = extents.max_rows.max(run.row.saturating_add(1));
        }
    }
    extents
}

/// Java: TerminalRecordingService#even — clamps to >= 2 and rounds up to an
/// even value (yuv420p requires even dimensions).
fn even(value: u32) -> u32 {
    let safe = value.max(2);
    if safe.is_multiple_of(2) {
        safe
    } else {
        safe + 1
    }
}

fn compute_render_dimensions(
    extents: FrameExtents,
    cell_width: u32,
    line_height: u32,
) -> (u32, u32) {
    let computed_width =
        (FRAME_PADDING_X * 2).saturating_add(cell_width.saturating_mul(extents.max_columns));
    let computed_height =
        (FRAME_PADDING_Y * 2).saturating_add(line_height.saturating_mul(extents.max_rows));
    (
        even(computed_width.max(extents.max_pixel_width)),
        even(computed_height.max(extents.max_pixel_height)),
    )
}

/// Java: TerminalRecordingService#resolveRenderLayout.
fn resolve_render_layout(
    frames: &[TerminalRecordingReplayFrame],
    fonts: &ExportFonts,
) -> RenderLayout {
    let scaled = fonts.regular.as_scaled(PxScale::from(EXPORT_FONT_SIZE));
    let cell_width = (scaled.h_advance(fonts.regular.glyph_id('W')).ceil() as u32).max(1);
    let line_height =
        ((scaled.ascent() - scaled.descent() + scaled.line_gap()).ceil() as u32).max(1);
    let ascent = scaled.ascent().ceil() as u32;
    let (width, height) =
        compute_render_dimensions(collect_frame_extents(frames), cell_width, line_height);
    RenderLayout {
        width,
        height,
        cell_width,
        line_height,
        ascent,
    }
}

/// Java: TerminalRecordingService#splitScreenLines (split keeps trailing empties).
fn split_screen_lines(screen: &str) -> Vec<String> {
    screen
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .map(ToString::to_string)
        .collect()
}

fn baseline(row: u32, layout: &RenderLayout) -> u32 {
    FRAME_PADDING_Y
        .saturating_add(row.saturating_mul(layout.line_height))
        .saturating_add(layout.ascent)
}

/// Java: TerminalRecordingService#parseColor (Color.decode with fallback).
fn parse_color(value: Option<&str>, fallback: Rgb) -> Rgb {
    let Some(value) = value
        .map(str::trim)
        .filter(|candidate| !candidate.is_empty())
    else {
        return fallback;
    };
    let parsed = if let Some(hex) = value.strip_prefix('#') {
        u32::from_str_radix(hex, 16).ok()
    } else if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        u32::from_str_radix(hex, 16).ok()
    } else {
        value.parse::<u32>().ok()
    };
    match parsed {
        Some(rgb) => [
            ((rgb >> 16) & 0xFF) as u8,
            ((rgb >> 8) & 0xFF) as u8,
            (rgb & 0xFF) as u8,
        ],
        None => fallback,
    }
}

/// RGB8 frame buffer with a black background.
struct FrameImage {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl FrameImage {
    fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; width as usize * height as usize * 3],
        }
    }

    #[cfg(test)]
    fn pixel(&self, x: u32, y: u32) -> Rgb {
        let index = (y as usize * self.width as usize + x as usize) * 3;
        [
            self.pixels[index],
            self.pixels[index + 1],
            self.pixels[index + 2],
        ]
    }

    fn fill_rect(&mut self, x: u32, y: u32, rect_width: u32, rect_height: u32, color: Rgb) {
        let x_end = x.saturating_add(rect_width).min(self.width);
        let y_end = y.saturating_add(rect_height).min(self.height);
        for row in y.min(self.height)..y_end {
            for column in x.min(self.width)..x_end {
                let index = (row as usize * self.width as usize + column as usize) * 3;
                self.pixels[index..index + 3].copy_from_slice(&color);
            }
        }
    }

    /// Alpha-blends one antialiased glyph pixel onto the buffer.
    fn blend_pixel(&mut self, x: i64, y: i64, color: Rgb, coverage: f32) {
        if x < 0 || y < 0 || x >= i64::from(self.width) || y >= i64::from(self.height) {
            return;
        }
        let coverage = coverage.clamp(0.0, 1.0);
        let index = (y as usize * self.width as usize + x as usize) * 3;
        for (channel, target) in color.iter().enumerate() {
            let existing = f32::from(self.pixels[index + channel]);
            let target = f32::from(*target);
            self.pixels[index + channel] =
                (existing + (target - existing) * coverage).round() as u8;
        }
    }

    fn write_png(&self, path: &Path) -> Result<()> {
        let file = File::create(path)
            .with_context(|| format!("Failed to create frame image {}", path.display()))?;
        let mut encoder = png::Encoder::new(BufWriter::new(file), self.width, self.height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&self.pixels)?;
        writer.finish()?;
        Ok(())
    }
}

/// Rasterizes `text` on the monospace cell grid starting at `x` with ab_glyph.
fn draw_text(
    image: &mut FrameImage,
    font: &FontRef<'static>,
    text: &str,
    x: u32,
    baseline_y: u32,
    cell_width: u32,
    color: Rgb,
) {
    let scale = PxScale::from(EXPORT_FONT_SIZE);
    let mut pen_x = x as f32;
    for ch in text.chars() {
        let glyph = font
            .glyph_id(ch)
            .with_scale_and_position(scale, ab_glyph::point(pen_x, baseline_y as f32));
        if let Some(outlined) = font.outline_glyph(glyph) {
            let bounds = outlined.px_bounds();
            outlined.draw(|gx, gy, coverage| {
                image.blend_pixel(
                    i64::from(bounds.min.x as i32) + i64::from(gx),
                    i64::from(bounds.min.y as i32) + i64::from(gy),
                    color,
                    coverage,
                );
            });
        }
        pen_x += cell_width as f32;
    }
}

/// Java: TerminalRecordingService#renderFrame (without the PNG write).
fn render_frame_image(
    frame: &TerminalRecordingReplayFrame,
    layout: &RenderLayout,
    include_color: bool,
    fonts: &ExportFonts,
) -> FrameImage {
    // The zero-initialized buffer is the black background fill.
    let mut image = FrameImage::new(layout.width, layout.height);
    if include_color && !frame.style_runs.is_empty() {
        render_styled_frame(&mut image, frame, layout, fonts);
    } else {
        render_plain_frame(&mut image, &frame.content, layout, fonts);
    }
    image
}

/// Java: TerminalRecordingService#renderPlainFrame.
fn render_plain_frame(
    image: &mut FrameImage,
    screen: &str,
    layout: &RenderLayout,
    fonts: &ExportFonts,
) {
    for (row, line) in split_screen_lines(screen).iter().enumerate() {
        draw_text(
            image,
            &fonts.regular,
            line,
            FRAME_PADDING_X,
            baseline(row as u32, layout),
            layout.cell_width,
            EXPORT_FOREGROUND,
        );
    }
}

/// Java: TerminalRecordingService#renderStyledFrame.
fn render_styled_frame(
    image: &mut FrameImage,
    frame: &TerminalRecordingReplayFrame,
    layout: &RenderLayout,
    fonts: &ExportFonts,
) {
    for run in &frame.style_runs {
        let x = FRAME_PADDING_X.saturating_add(run.column.saturating_mul(layout.cell_width));
        let y = FRAME_PADDING_Y.saturating_add(run.row.saturating_mul(layout.line_height));
        let mut foreground = parse_color(run.foreground.as_deref(), EXPORT_FOREGROUND);
        let mut background = parse_color(run.background.as_deref(), EXPORT_BACKGROUND);
        if run.options.iter().any(|option| option == "INVERSE") {
            std::mem::swap(&mut foreground, &mut background);
        }
        let text_cells = run.text.chars().count() as u32;
        image.fill_rect(
            x,
            y,
            layout
                .cell_width
                .max(layout.cell_width.saturating_mul(text_cells)),
            layout.line_height,
            background,
        );
        if !run.options.iter().any(|option| option == "HIDDEN") {
            let font = if run.options.iter().any(|option| option == "BOLD") {
                &fonts.bold
            } else {
                &fonts.regular
            };
            draw_text(
                image,
                font,
                &run.text,
                x,
                baseline(run.row, layout),
                layout.cell_width,
                foreground,
            );
        }
    }
}

/// Java: TerminalRecordingService#renderThreadCount (max 4 render threads).
fn render_thread_count(frame_count: usize) -> usize {
    let parallelism = std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(1);
    parallelism
        .saturating_sub(1)
        .clamp(1, MAX_RENDER_THREADS)
        .min(frame_count.max(1))
}

/// Java: TerminalRecordingService#renderFrameImages — renders all frames as
/// PNGs in parallel and reports 0..RENDER_PROGRESS_WEIGHT progress.
fn render_frame_images(
    frame_dir: &Path,
    frames: &[TerminalRecordingReplayFrame],
    layout: &RenderLayout,
    include_color: bool,
    fonts: &ExportFonts,
    progress: Option<&ExportProgressListener>,
) -> Result<Vec<PathBuf>> {
    let frame_files: Vec<PathBuf> = (0..frames.len())
        .map(|index| frame_dir.join(format!("frame-{:05}.png", index + 1)))
        .collect();
    let next_index = AtomicUsize::new(0);
    let completed = AtomicUsize::new(0);
    let failure: Mutex<Option<anyhow::Error>> = Mutex::new(None);
    std::thread::scope(|scope| {
        for _ in 0..render_thread_count(frames.len()) {
            scope.spawn(|| loop {
                let index = next_index.fetch_add(1, Ordering::SeqCst);
                if index >= frames.len() {
                    break;
                }
                if failure.lock().map(|guard| guard.is_some()).unwrap_or(true) {
                    break;
                }
                let image = render_frame_image(&frames[index], layout, include_color, fonts);
                if let Err(error) = image.write_png(&frame_files[index]) {
                    if let Ok(mut guard) = failure.lock() {
                        guard.get_or_insert(error);
                    }
                    break;
                }
                let done = completed.fetch_add(1, Ordering::SeqCst) + 1;
                report_progress(
                    progress,
                    TerminalRecordingExportPhase::Rendering,
                    done as f64 / frames.len() as f64 * RENDER_PROGRESS_WEIGHT,
                );
            });
        }
    });
    if let Ok(mut guard) = failure.lock() {
        if let Some(error) = guard.take() {
            return Err(error);
        }
    }
    Ok(frame_files)
}

/// Java: TerminalRecordingService#renderReplayFrames concat file content —
/// `file '…'` + `duration %.6f` per frame, with the last frame repeated so the
/// concat demuxer honors the final duration.
fn build_concat_file_content(
    frame_files: &[PathBuf],
    frames: &[TerminalRecordingReplayFrame],
) -> String {
    let mut content = String::new();
    for (frame_file, frame) in frame_files.iter().zip(frames) {
        content.push_str("file '");
        content.push_str(&escape_concat_path(frame_file));
        content.push_str("'\n");
        content.push_str(&format!("duration {:.6}\n", frame_duration_seconds(frame)));
    }
    if let Some(last) = frame_files.last() {
        content.push_str("file '");
        content.push_str(&escape_concat_path(last));
        content.push_str("'\n");
    }
    content
}

/// Java: TerminalRecordingService#escapeConcatPath ('\''-escaping).
fn escape_concat_path(path: &Path) -> String {
    path.to_string_lossy().replace('\'', "'\\''")
}

/// Java: TerminalRecordingService#buildFfmpegCommand + process handling.
fn run_ffmpeg_export(
    ffmpeg: &str,
    concat_file: &Path,
    target: &Path,
    format: &TerminalRecordingExportFormat,
    total_duration_seconds: f64,
    progress: Option<&ExportProgressListener>,
) -> Result<()> {
    let mut command = Command::new(ffmpeg);
    command
        .arg("-y")
        .arg("-nostats")
        .args(["-progress", "pipe:1"])
        .args(["-f", "concat"])
        .args(["-safe", "0"])
        .arg("-i")
        .arg(concat_file)
        .args(["-fps_mode:v", "vfr"])
        .args(["-threads", "0"]);
    match format {
        TerminalRecordingExportFormat::Mkv => {
            command.args(["-c:v", "ffv1", "-level", "3", "-g", "1"]);
        }
        TerminalRecordingExportFormat::Webm => {
            command.args(["-c:v", "libvpx-vp9", "-row-mt", "1", "-cpu-used", "4"]);
            command.args(["-pix_fmt", "yuv420p"]);
        }
    }
    command
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .with_context(|| format!("Failed to run ffmpeg at '{ffmpeg}'"))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let output = Arc::new(Mutex::new(BoundedProcessOutput::new()));

    let status = std::thread::scope(|scope| -> Result<std::process::ExitStatus> {
        if let Some(stdout) = stdout {
            let output = Arc::clone(&output);
            scope.spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(|line| line.ok()) {
                    handle_ffmpeg_progress_line(&line, total_duration_seconds, progress);
                    if let Ok(mut guard) = output.lock() {
                        guard.append_line(&line);
                    }
                }
            });
        }
        if let Some(stderr) = stderr {
            let output = Arc::clone(&output);
            scope.spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(|line| line.ok()) {
                    if let Ok(mut guard) = output.lock() {
                        guard.append_line(&line);
                    }
                }
            });
        }
        let deadline = Instant::now() + FFMPEG_EXPORT_TIMEOUT;
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(anyhow!(
                    "ffmpeg timed out after {} seconds: {}",
                    FFMPEG_EXPORT_TIMEOUT.as_secs(),
                    output
                        .lock()
                        .map(|guard| guard.as_string())
                        .unwrap_or_default()
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    })?;
    if !status.success() {
        return Err(anyhow!(
            "ffmpeg failed with exit code {}: {}",
            status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            output
                .lock()
                .map(|guard| guard.as_string())
                .unwrap_or_default()
        ));
    }
    Ok(())
}

/// Java: TerminalRecordingService#handleFfmpegProgressLine — maps the
/// `out_time=HH:MM:SS.xx` progress lines onto the encoding fraction.
fn handle_ffmpeg_progress_line(
    line: &str,
    total_duration_seconds: f64,
    progress: Option<&ExportProgressListener>,
) {
    if total_duration_seconds <= 0.0 {
        return;
    }
    let Some(value) = line.strip_prefix("out_time=") else {
        return;
    };
    let Some(encoded_seconds) = parse_ffmpeg_out_time_seconds(value) else {
        return;
    };
    let encode_progress = (encoded_seconds / total_duration_seconds).min(1.0);
    report_progress(
        progress,
        TerminalRecordingExportPhase::Encoding,
        RENDER_PROGRESS_WEIGHT + encode_progress * ENCODE_PROGRESS_WEIGHT,
    );
}

/// Java: TerminalRecordingService#parseFfmpegOutTimeSeconds.
fn parse_ffmpeg_out_time_seconds(value: &str) -> Option<f64> {
    let parts: Vec<&str> = value.trim().split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    let hours: f64 = parts[0].parse().ok()?;
    let minutes: f64 = parts[1].parse().ok()?;
    let seconds: f64 = parts[2].parse().ok()?;
    let total = hours * 3600.0 + minutes * 60.0 + seconds;
    if total < 0.0 {
        None
    } else {
        Some(total)
    }
}

/// Java: TerminalRecordingService.BoundedProcessOutput (max 64KB buffered).
struct BoundedProcessOutput {
    buffer: Vec<u8>,
    truncated: bool,
}

impl BoundedProcessOutput {
    fn new() -> Self {
        Self {
            buffer: Vec::new(),
            truncated: false,
        }
    }

    fn append_line(&mut self, line: &str) {
        let bytes = line.as_bytes();
        let needed = bytes.len() + 1;
        let remaining = MAX_PROCESS_OUTPUT_BYTES.saturating_sub(self.buffer.len());
        if remaining > 0 {
            let take = bytes.len().min(remaining);
            self.buffer.extend_from_slice(&bytes[..take]);
            if self.buffer.len() < MAX_PROCESS_OUTPUT_BYTES {
                self.buffer.push(b'\n');
            }
        }
        if needed > remaining {
            self.truncated = true;
        }
    }

    fn as_string(&self) -> String {
        let mut text = String::from_utf8_lossy(&self.buffer).into_owned();
        if self.truncated {
            text.push_str(&format!(
                "\n[output truncated after {MAX_PROCESS_OUTPUT_BYTES} bytes]"
            ));
        }
        text
    }
}

fn summarize_replay(path: &Path) -> Result<TerminalRecordingReplaySummary> {
    ensure_replay_path(path)?;
    let metadata = fs::metadata(path)?;
    let events = read_replay_events(path)?;
    let started_at_millis = events
        .iter()
        .find(|event| matches!(event.event_type.as_str(), "metadata" | "session_created"))
        .map(|event| event.at_millis)
        .or_else(|| events.first().map(|event| event.at_millis));
    let ended_at_millis = events
        .iter()
        .rev()
        .find(|event| {
            matches!(
                event.event_type.as_str(),
                "stop"
                    | "snapshot"
                    | "input"
                    | "recording_stop"
                    | "screen"
                    | "user_input_activity"
                    | "session_closed"
            )
        })
        .map(|event| event.at_millis);
    Ok(TerminalRecordingReplaySummary {
        id: path.to_string_lossy().to_string(),
        name: replay_display_name(path),
        file_path: path.to_string_lossy().to_string(),
        size_bytes: metadata.len(),
        compressed: is_gzip_replay(path),
        started_at_millis,
        ended_at_millis,
        duration_millis: started_at_millis
            .zip(ended_at_millis)
            .map(|(started, ended)| ended.saturating_sub(started)),
        event_count: events.len(),
    })
}

fn parse_iso_millis(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.timestamp_millis())
}

/// Reads replay events in both the Java format ({"type", "at": ISO-8601,
/// "content", ...}) and the legacy Rust format ({"eventType", "atMillis",
/// "text", ...}).
fn read_replay_events(path: &Path) -> Result<Vec<TerminalRecordingReplayEvent>> {
    ensure_replay_path(path)?;
    let file = File::open(path)?;
    let reader: Box<dyn BufRead> = if is_gzip_replay(path) {
        Box::new(BufReader::new(GzDecoder::new(file)))
    } else {
        Box::new(BufReader::new(file))
    };
    let mut events = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(&line)?;
        events.push(TerminalRecordingReplayEvent {
            event_type: string_field(&value, "type")
                .or_else(|| string_field(&value, "eventType"))
                .unwrap_or_else(|| "snapshot".into()),
            at_millis: value
                .get("at")
                .and_then(Value::as_str)
                .and_then(parse_iso_millis)
                .or_else(|| value.get("atMillis").and_then(Value::as_i64))
                .or_else(|| value.get("timestamp").and_then(Value::as_i64))
                .unwrap_or_default(),
            text: string_field(&value, "content")
                .or_else(|| string_field(&value, "text"))
                .or_else(|| string_field(&value, "data")),
            columns: u16_field(&value, "columns"),
            rows: u16_field(&value, "rows"),
            cursor_column: u16_field(&value, "cursorColumn"),
            cursor_row: u16_field(&value, "cursorRow"),
            widget: string_field(&value, "widget"),
            pixel_width: u32_field(&value, "pixelWidth"),
            pixel_height: u32_field(&value, "pixelHeight"),
            style_runs: parse_style_runs(&value),
        });
    }
    Ok(events)
}

fn parse_style_runs(value: &Value) -> Vec<TerminalRecordingStyleRun> {
    let Some(array) = value.get("styleRuns").and_then(Value::as_array) else {
        return Vec::new();
    };
    array
        .iter()
        .filter_map(|run| {
            let text = run.get("text")?.as_str()?;
            if text.is_empty() {
                return None;
            }
            Some(TerminalRecordingStyleRun {
                row: u32_field(run, "row").unwrap_or(0),
                column: u32_field(run, "column").unwrap_or(0),
                text: text.to_string(),
                foreground: string_field(run, "foreground"),
                background: string_field(run, "background"),
                options: run
                    .get("options")
                    .and_then(Value::as_array)
                    .map(|options| {
                        options
                            .iter()
                            .filter_map(|option| option.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default(),
            })
        })
        .collect()
}

/// Maps legacy Rust event names onto the Java replay semantics so that legacy
/// recordings keep producing a sensible timeline.
fn normalize_event_type(event_type: &str) -> &str {
    match event_type {
        // Legacy recordings start recording immediately with the metadata event.
        "metadata" => "recording_start",
        "snapshot" => "screen",
        "pause" => "auto_pause",
        "resume" => "auto_resume",
        "stop" => "recording_stop",
        other => other,
    }
}

fn screen_snapshot_from_event(event: &TerminalRecordingReplayEvent) -> Option<ScreenSnapshot> {
    let content = event.text.clone()?;
    Some(ScreenSnapshot {
        content,
        columns: event.columns.map(u32::from).unwrap_or(0),
        rows: event.rows.map(u32::from).unwrap_or(0),
        pixel_width: event.pixel_width.unwrap_or(0),
        pixel_height: event.pixel_height.unwrap_or(0),
        style_runs: event.style_runs.clone(),
    })
}

struct ReplayTimelineData {
    frames: Vec<(ScreenSnapshot, i64)>,
    end_playback_millis: i64,
}

/// Java: TerminalRecordingService#readReplayTimeline. The replay clock only
/// advances between recording_start/recording_stop and outside of
/// auto_pause/auto_resume spans.
fn read_replay_timeline(events: &[TerminalRecordingReplayEvent]) -> ReplayTimelineData {
    let mut frames: Vec<(ScreenSnapshot, i64)> = Vec::new();
    let mut recording = false;
    let mut paused = false;
    let mut last_active_at: Option<i64> = None;
    let mut playback_millis: i64 = 0;
    let mut end_playback_millis: i64 = 0;
    let mut previous_snapshot: Option<ScreenSnapshot> = None;

    for event in events {
        if recording && !paused {
            if let Some(last) = last_active_at {
                playback_millis += (event.at_millis - last).max(0);
                last_active_at = Some(event.at_millis);
                end_playback_millis = playback_millis;
            }
        }
        match normalize_event_type(&event.event_type) {
            "recording_start" => {
                recording = true;
                paused = false;
                last_active_at = Some(event.at_millis);
            }
            "auto_pause" => {
                if recording && !paused {
                    paused = true;
                    last_active_at = None;
                }
            }
            "auto_resume" => {
                if recording {
                    paused = false;
                    last_active_at = Some(event.at_millis);
                }
            }
            "recording_stop" => {
                recording = false;
                paused = false;
                last_active_at = None;
                end_playback_millis = playback_millis;
            }
            "screen" if recording => {
                if let Some(snapshot) = screen_snapshot_from_event(event) {
                    if previous_snapshot.as_ref() != Some(&snapshot) {
                        frames.push((snapshot.clone(), playback_millis));
                        previous_snapshot = Some(snapshot);
                    }
                }
            }
            _ => {
                // Non-visual events still advance the active playback clock above.
            }
        }
    }
    ReplayTimelineData {
        frames,
        end_playback_millis,
    }
}

fn frame_duration_from_millis(duration_millis: i64) -> f64 {
    (duration_millis as f64 / 1000.0).max(MIN_FRAME_DURATION_SECONDS)
}

/// Java: TerminalRecordingService#buildTimedReplayFrames.
pub fn build_timed_replay_frames(path: &Path) -> Result<Vec<TerminalRecordingReplayFrame>> {
    let events = read_replay_events(path)?;
    let timeline = read_replay_timeline(&events);
    if timeline.frames.is_empty() {
        return Ok(Vec::new());
    }
    let first_frame_millis = timeline.frames[0].1;
    let normalized_end_millis = (timeline.end_playback_millis - first_frame_millis).max(0);
    let mut timed_frames = Vec::with_capacity(timeline.frames.len());
    for (index, (snapshot, playback_millis)) in timeline.frames.iter().enumerate() {
        let current_millis = (playback_millis - first_frame_millis).max(0);
        let has_next = index + 1 < timeline.frames.len();
        let next_millis = if has_next {
            (timeline.frames[index + 1].1 - first_frame_millis).max(0)
        } else {
            normalized_end_millis
        };
        let duration_seconds = if has_next || next_millis > current_millis {
            frame_duration_from_millis(next_millis - current_millis)
        } else {
            FALLBACK_LAST_FRAME_DURATION_SECONDS
        };
        timed_frames.push(snapshot.to_frame(duration_seconds));
    }
    Ok(timed_frames)
}

/// Clamped frame duration (Java: TerminalRecordingReplayTimeline#durationSeconds).
pub fn frame_duration_seconds(frame: &TerminalRecordingReplayFrame) -> f64 {
    if !frame.duration_seconds.is_finite() {
        return MIN_FRAME_DURATION_SECONDS;
    }
    frame.duration_seconds.max(MIN_FRAME_DURATION_SECONDS)
}

pub fn total_duration_seconds(frames: &[TerminalRecordingReplayFrame]) -> f64 {
    frames.iter().map(frame_duration_seconds).sum()
}

/// Java: TerminalRecordingService#sliceReplayFrames. Pass
/// `(0.0, f64::INFINITY)` for the full range.
pub fn slice_replay_frames(
    frames: &[TerminalRecordingReplayFrame],
    start_seconds: f64,
    end_seconds: f64,
) -> Result<Vec<TerminalRecordingReplayFrame>> {
    if frames.is_empty() {
        return Ok(Vec::new());
    }
    let total = total_duration_seconds(frames);
    let total = if total.is_finite() {
        total.max(0.0)
    } else {
        0.0
    };
    let start = if start_seconds.is_finite() {
        start_seconds.max(0.0)
    } else {
        0.0
    }
    .min(total);
    let end = if end_seconds.is_finite() {
        end_seconds
    } else {
        total
    }
    .max(0.0)
    .min(total);
    if end <= start {
        return Err(anyhow!("Export time range is outside the replay duration"));
    }

    let mut sliced = Vec::new();
    let mut cursor_seconds = 0.0_f64;
    for frame in frames {
        let frame_duration = frame_duration_seconds(frame);
        let frame_start = cursor_seconds;
        let frame_end = frame_start + frame_duration;
        let overlap_start = frame_start.max(start);
        let overlap_end = frame_end.min(end);
        if overlap_end > overlap_start {
            let mut slice = frame.clone();
            slice.duration_seconds = overlap_end - overlap_start;
            sliced.push(slice);
        }
        cursor_seconds = frame_end;
        if cursor_seconds >= end {
            break;
        }
    }
    Ok(sliced)
}

fn is_replay_path(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    name.ends_with(".korttyrec.jsonl") || name.ends_with(".korttyrec.jsonl.gz")
}

fn ensure_replay_path(path: &Path) -> Result<()> {
    if !is_replay_path(path) {
        return Err(anyhow!("Unsupported terminal replay file"));
    }
    Ok(())
}

fn is_gzip_replay(path: &Path) -> bool {
    path.file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|name| name.ends_with(".gz"))
}

fn replay_extension(path: &Path) -> Option<&'static str> {
    let name = path.file_name()?.to_str()?;
    if name.ends_with(".korttyrec.jsonl.gz") {
        Some(".korttyrec.jsonl.gz")
    } else if name.ends_with(".korttyrec.jsonl") {
        Some(".korttyrec.jsonl")
    } else {
        None
    }
}

fn replay_display_name(path: &Path) -> String {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("terminal replay");
    name.trim_end_matches(".gz")
        .trim_end_matches(".jsonl")
        .trim_end_matches(".korttyrec")
        .to_string()
}

fn validate_video_extension(path: &Path, format: &TerminalRecordingExportFormat) -> Result<()> {
    let expected = match format {
        TerminalRecordingExportFormat::Webm => "webm",
        TerminalRecordingExportFormat::Mkv => "mkv",
    };
    if path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(expected))
    {
        Ok(())
    } else {
        Err(anyhow!("Target path must end with .{expected}"))
    }
}

fn find_tool(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key)?.as_str().map(ToString::to_string)
}

fn u16_field(value: &Value, key: &str) -> Option<u16> {
    value
        .get(key)?
        .as_u64()
        .and_then(|number| u16::try_from(number).ok())
}

fn u32_field(value: &Value, key: &str) -> Option<u32> {
    value
        .get(key)?
        .as_u64()
        .and_then(|number| u32::try_from(number).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn temp_dir(prefix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4().simple()));
        fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn new_session(
        dir: &Path,
        auto_pause_enabled: bool,
        idle_pause_seconds: u32,
    ) -> TerminalRecordingSession {
        TerminalRecordingSession::create(
            "session-id".into(),
            "tab-1".into(),
            Some("split-1".into()),
            dir.join("session.korttyrec.jsonl.gz"),
            "prod",
            auto_pause_enabled,
            idle_pause_seconds,
            now_millis(),
            None,
        )
        .expect("session")
    }

    fn read_gz_replay(path: &Path) -> String {
        let mut replay = String::new();
        GzDecoder::new(File::open(path).expect("open replay"))
            .read_to_string(&mut replay)
            .expect("read replay");
        replay
    }

    fn count_occurrences(text: &str, needle: &str) -> usize {
        text.matches(needle).count()
    }

    fn write_plain_replay(dir: &Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, content).expect("write replay fixture");
        path
    }

    // Fixture lines taken from the Java TerminalRecordingServiceTest.
    const JAVA_TIMED_REPLAY: &str = concat!(
        "{\"type\":\"session_created\",\"at\":\"2026-05-13T12:00:00Z\",\"connection\":\"prod\",\"formatVersion\":1}\n",
        "{\"type\":\"recording_start\",\"at\":\"2026-05-13T12:00:00Z\",\"scope\":\"ACTIVE_SPLIT\",\"segment\":1}\n",
        "{\"type\":\"screen\",\"at\":\"2026-05-13T12:00:00Z\",\"widget\":\"split-1\",\"content\":\"one\"}\n",
        "{\"type\":\"screen\",\"at\":\"2026-05-13T12:00:00.100Z\",\"widget\":\"split-1\",\"content\":\"two\"}\n",
        "{\"type\":\"auto_pause\",\"at\":\"2026-05-13T12:00:01.100Z\",\"idleMillis\":1000}\n",
        "{\"type\":\"auto_resume\",\"at\":\"2026-05-13T12:01:01.100Z\",\"source\":\"screen\"}\n",
        "{\"type\":\"screen\",\"at\":\"2026-05-13T12:01:01.100Z\",\"widget\":\"split-1\",\"content\":\"three\"}\n",
        "{\"type\":\"recording_stop\",\"at\":\"2026-05-13T12:01:01.600Z\",\"segment\":1}\n",
    );

    const JAVA_STYLED_REPLAY: &str = concat!(
        "{\"type\":\"session_created\",\"at\":\"2026-05-13T12:00:00Z\",\"connection\":\"prod\",\"formatVersion\":1}\n",
        "{\"type\":\"recording_start\",\"at\":\"2026-05-13T12:00:00Z\",\"scope\":\"ACTIVE_SPLIT\",\"segment\":1}\n",
        "{\"type\":\"screen\",\"at\":\"2026-05-13T12:00:00Z\",\"widget\":\"split-1\",\"content\":\"red\",\"columns\":120,\"rows\":40,\"pixelWidth\":1600,\"pixelHeight\":900,\"styleRuns\":[{\"row\":0,\"column\":0,\"text\":\"red\",\"foreground\":\"#FF0000\",\"background\":\"#000000\",\"options\":[\"BOLD\"]}]}\n",
        "{\"type\":\"recording_stop\",\"at\":\"2026-05-13T12:00:01Z\",\"segment\":1}\n",
    );

    #[test]
    fn accepts_current_and_legacy_replay_extensions() {
        assert!(is_replay_path(Path::new("a.korttyrec.jsonl")));
        assert!(is_replay_path(Path::new("a.korttyrec.jsonl.gz")));
        assert!(!is_replay_path(Path::new("a.json")));
    }

    #[test]
    fn sanitize_file_name_keeps_only_portable_characters() {
        assert_eq!(
            sanitize_file_name("prod / root@host:22"),
            "prod_root_host_22"
        );
        assert_eq!(sanitize_file_name("   "), "terminal");
        assert_eq!(sanitize_file_name("///"), "terminal");
        assert_eq!(sanitize_file_name("a__b"), "a_b");
        assert_eq!(sanitize_file_name("_edge_"), "edge");
    }

    #[test]
    fn next_replay_file_appends_collision_counter() {
        let dir = temp_dir("kortty-recording-name");
        let first = next_replay_file(&dir, "prod/server", "session-1234567890");
        let name = first.file_name().unwrap().to_str().unwrap().to_string();
        assert!(name.starts_with("prod_server-"));
        assert!(name.ends_with("-session-1234.korttyrec.jsonl.gz"));
        fs::write(&first, b"x").unwrap();
        let second = next_replay_file(&dir, "prod/server", "session-1234567890");
        let second_name = second.file_name().unwrap().to_str().unwrap().to_string();
        assert!(second_name.ends_with("-session-1234-2.korttyrec.jsonl.gz"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reads_java_format_replay_events() {
        let dir = temp_dir("kortty-java-read");
        let path = write_plain_replay(&dir, "java.korttyrec.jsonl", JAVA_TIMED_REPLAY);
        let events = read_replay_events(&path).expect("events");
        assert_eq!(events.len(), 8);
        assert_eq!(events[0].event_type, "session_created");
        // 2026-05-13T12:00:00Z in epoch millis.
        assert_eq!(events[0].at_millis, 1_778_673_600_000);
        assert_eq!(events[3].event_type, "screen");
        assert_eq!(events[3].at_millis, 1_778_673_600_100);
        assert_eq!(events[3].text.as_deref(), Some("two"));
        assert_eq!(events[3].widget.as_deref(), Some("split-1"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reads_legacy_rust_format_replay_events() {
        let dir = temp_dir("kortty-legacy-read");
        let path = write_plain_replay(
            &dir,
            "legacy.korttyrec.jsonl",
            concat!(
                "{\"eventType\":\"metadata\",\"version\":2,\"atMillis\":1000,\"sessionId\":\"s\",\"tabId\":\"t\"}\n",
                "{\"eventType\":\"snapshot\",\"atMillis\":1500,\"text\":\"hello\",\"columns\":80,\"rows\":24}\n",
                "{\"eventType\":\"input\",\"atMillis\":1600,\"text\":\"ls\"}\n",
                "{\"eventType\":\"stop\",\"atMillis\":2000,\"startedAtMillis\":1000}\n",
            ),
        );
        let events = read_replay_events(&path).expect("events");
        assert_eq!(events.len(), 4);
        assert_eq!(events[0].event_type, "metadata");
        assert_eq!(events[0].at_millis, 1000);
        assert_eq!(events[1].event_type, "snapshot");
        assert_eq!(events[1].text.as_deref(), Some("hello"));
        assert_eq!(events[1].columns, Some(80));
        assert_eq!(events[3].event_type, "stop");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn timed_replay_frames_use_replay_timing_and_skip_paused_time() {
        let dir = temp_dir("kortty-timed");
        let path = write_plain_replay(&dir, "timed.korttyrec.jsonl", JAVA_TIMED_REPLAY);
        let frames = build_timed_replay_frames(&path).expect("frames");
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0].content, "one");
        assert!((frames[0].duration_seconds - 0.1).abs() < 0.001);
        assert_eq!(frames[1].content, "two");
        assert!((frames[1].duration_seconds - 1.0).abs() < 0.001);
        assert_eq!(frames[2].content, "three");
        assert!((frames[2].duration_seconds - 0.5).abs() < 0.001);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn timed_replay_frames_preserve_geometry_and_color_runs() {
        let dir = temp_dir("kortty-styled");
        let path = write_plain_replay(&dir, "styled.korttyrec.jsonl", JAVA_STYLED_REPLAY);
        let frames = build_timed_replay_frames(&path).expect("frames");
        assert_eq!(frames.len(), 1);
        let frame = &frames[0];
        assert_eq!(frame.columns, 120);
        assert_eq!(frame.rows, 40);
        assert_eq!(frame.pixel_width, 1600);
        assert_eq!(frame.pixel_height, 900);
        assert_eq!(frame.style_runs.len(), 1);
        assert_eq!(frame.style_runs[0].foreground.as_deref(), Some("#FF0000"));
        assert!(frame.style_runs[0].options.contains(&"BOLD".to_string()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn timed_replay_frames_read_compressed_replay() {
        let dir = temp_dir("kortty-timed-gz");
        let path = dir.join("timed.korttyrec.jsonl.gz");
        let mut encoder =
            GzEncoder::new(File::create(&path).expect("create"), Compression::default());
        encoder
            .write_all(
                concat!(
                    "{\"type\":\"session_created\",\"at\":\"2026-05-13T12:00:00Z\",\"connection\":\"prod\",\"formatVersion\":1}\n",
                    "{\"type\":\"recording_start\",\"at\":\"2026-05-13T12:00:00Z\",\"scope\":\"ACTIVE_SPLIT\",\"segment\":1}\n",
                    "{\"type\":\"screen\",\"at\":\"2026-05-13T12:00:00Z\",\"widget\":\"split-1\",\"content\":\"one\"}\n",
                    "{\"type\":\"recording_stop\",\"at\":\"2026-05-13T12:00:01Z\",\"segment\":1}\n",
                )
                .as_bytes(),
            )
            .expect("write");
        encoder.finish().expect("finish");
        let frames = build_timed_replay_frames(&path).expect("frames");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].content, "one");
        assert!((frames[0].duration_seconds - 1.0).abs() < 0.001);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn slice_replay_frames_keeps_only_requested_range() {
        let frames = vec![
            ScreenSnapshot::plain("one").to_frame(2.0),
            ScreenSnapshot::plain("two").to_frame(3.0),
            ScreenSnapshot::plain("three").to_frame(4.0),
        ];
        let sliced = slice_replay_frames(&frames, 1.0, 6.0).expect("sliced");
        assert_eq!(sliced.len(), 3);
        assert_eq!(sliced[0].content, "one");
        assert!((sliced[0].duration_seconds - 1.0).abs() < 0.0001);
        assert_eq!(sliced[1].content, "two");
        assert!((sliced[1].duration_seconds - 3.0).abs() < 0.0001);
        assert_eq!(sliced[2].content, "three");
        assert!((sliced[2].duration_seconds - 1.0).abs() < 0.0001);
    }

    #[test]
    fn slice_replay_frames_rejects_range_outside_duration() {
        let frames = vec![ScreenSnapshot::plain("one").to_frame(2.0)];
        assert!(slice_replay_frames(&frames, 3.0, 5.0).is_err());
        assert!(slice_replay_frames(&frames, 1.5, 1.0).is_err());
    }

    #[test]
    fn slice_replay_frames_full_range_keeps_all_frames() {
        let frames = vec![
            ScreenSnapshot::plain("one").to_frame(2.0),
            ScreenSnapshot::plain("two").to_frame(3.0),
        ];
        let sliced = slice_replay_frames(&frames, 0.0, f64::INFINITY).expect("sliced");
        assert_eq!(sliced.len(), 2);
        assert!((total_duration_seconds(&sliced) - 5.0).abs() < 0.0001);
    }

    #[test]
    fn session_writes_java_format_events() {
        let dir = temp_dir("kortty-session-format");
        let mut session = new_session(&dir, true, 20);
        session
            .start(&TerminalRecordingScope::ActiveSplit)
            .expect("start");
        session
            .record_screen_snapshot("split-1", &ScreenSnapshot::plain("compressed\n"))
            .expect("snapshot");
        session.stop().expect("stop");
        let path = session.file_path.clone();
        session.close().expect("close");
        let replay = read_gz_replay(&path);
        assert!(replay.contains("\"type\":\"session_created\""));
        assert!(replay.contains("\"connection\":\"prod\""));
        assert!(replay.contains("\"formatVersion\":1"));
        assert!(replay.contains("\"type\":\"recording_start\""));
        assert!(replay.contains("\"scope\":\"ACTIVE_SPLIT\""));
        assert!(replay.contains("\"segment\":1"));
        assert!(replay.contains("\"type\":\"screen\""));
        assert!(replay.contains("\"widget\":\"split-1\""));
        assert!(replay.contains("\"content\":\"compressed\\n\""));
        assert!(replay.contains("\"type\":\"recording_stop\""));
        assert!(replay.contains("\"type\":\"session_closed\""));
        // Round-trip: the timed frame builder reads the session output back.
        let frames = build_timed_replay_frames(&path).expect("frames");
        assert_eq!(frames[0].content, "compressed\n");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn multiple_start_stop_segments_use_one_replay_file() {
        let dir = temp_dir("kortty-session-segments");
        let mut session = new_session(&dir, true, 20);
        session
            .start(&TerminalRecordingScope::ActiveSplit)
            .expect("start 1");
        session
            .record_screen_snapshot("split-1", &ScreenSnapshot::plain("one\n"))
            .expect("snapshot 1");
        session.stop().expect("stop 1");
        session
            .start(&TerminalRecordingScope::WholeTab)
            .expect("start 2");
        session
            .record_screen_snapshot("split-1", &ScreenSnapshot::plain("two\n"))
            .expect("snapshot 2");
        session.stop().expect("stop 2");
        let path = session.file_path.clone();
        session.close().expect("close");
        let replay = read_gz_replay(&path);
        assert_eq!(
            count_occurrences(&replay, "\"type\":\"session_created\""),
            1
        );
        assert_eq!(count_occurrences(&replay, "\"type\":\"session_closed\""), 1);
        assert_eq!(
            count_occurrences(&replay, "\"type\":\"recording_start\""),
            2
        );
        assert_eq!(count_occurrences(&replay, "\"type\":\"recording_stop\""), 2);
        assert_eq!(count_occurrences(&replay, "\"type\":\"screen\""), 2);
        assert!(replay.contains("\"scope\":\"ACTIVE_SPLIT\""));
        assert!(replay.contains("\"scope\":\"WHOLE_TAB\""));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn auto_pause_and_resume_on_screen_change() {
        let dir = temp_dir("kortty-autopause");
        let mut session = new_session(&dir, true, 20);
        session
            .start(&TerminalRecordingScope::ActiveSplit)
            .expect("start");
        let idle_at = session.last_activity_millis + 21_000;
        session.check_idle(idle_at).expect("check idle");
        assert_eq!(session.state, TerminalRecordingState::AutoPaused);

        session
            .record_screen_snapshot("split-1", &ScreenSnapshot::plain("new output\n"))
            .expect("snapshot");
        assert_eq!(session.state, TerminalRecordingState::Recording);

        let path = session.file_path.clone();
        session.close().expect("close");
        let replay = read_gz_replay(&path);
        assert!(replay.contains("\"type\":\"auto_pause\""));
        assert!(replay.contains("\"idleMillis\":21000"));
        assert!(replay.contains("\"type\":\"auto_resume\""));
        assert!(replay.contains("\"source\":\"screen\""));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn idle_check_is_skipped_when_auto_pause_disabled() {
        let dir = temp_dir("kortty-autopause-off");
        let mut session = new_session(&dir, false, 20);
        session
            .start(&TerminalRecordingScope::ActiveSplit)
            .expect("start");
        let idle_at = session.last_activity_millis + 60_000;
        session.check_idle(idle_at).expect("check idle");
        assert_eq!(session.state, TerminalRecordingState::Recording);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn user_input_activity_does_not_persist_input_text() {
        let dir = temp_dir("kortty-input");
        let mut session = new_session(&dir, true, 20);
        session
            .start(&TerminalRecordingScope::ActiveSplit)
            .expect("start");
        session.record_user_input_activity().expect("input");
        session.stop().expect("stop");
        let path = session.file_path.clone();
        session.close().expect("close");
        let replay = read_gz_replay(&path);
        assert!(replay.contains("\"type\":\"user_input_activity\""));
        assert!(!replay.contains("sudo"));
        assert!(!replay.contains("password"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn identical_screen_snapshots_are_deduplicated_per_widget() {
        let dir = temp_dir("kortty-dedup");
        let mut session = new_session(&dir, true, 20);
        session
            .start(&TerminalRecordingScope::ActiveSplit)
            .expect("start");
        session
            .record_screen_snapshot("split-1", &ScreenSnapshot::plain("same"))
            .expect("snapshot 1");
        session
            .record_screen_snapshot("split-1", &ScreenSnapshot::plain("same"))
            .expect("snapshot 2");
        // A different widget with the same content must still be written.
        session
            .record_screen_snapshot("split-2", &ScreenSnapshot::plain("same"))
            .expect("snapshot 3");
        let path = session.file_path.clone();
        session.close().expect("close");
        let replay = read_gz_replay(&path);
        assert_eq!(count_occurrences(&replay, "\"type\":\"screen\""), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn screen_snapshot_persists_geometry_and_optional_color_runs() {
        let dir = temp_dir("kortty-style");
        let mut session = new_session(&dir, true, 20);
        session
            .start(&TerminalRecordingScope::ActiveSplit)
            .expect("start");
        session
            .record_screen_snapshot(
                "split-1",
                &ScreenSnapshot {
                    content: "red".into(),
                    columns: 120,
                    rows: 40,
                    pixel_width: 1600,
                    pixel_height: 900,
                    style_runs: vec![TerminalRecordingStyleRun {
                        row: 0,
                        column: 0,
                        text: "red".into(),
                        foreground: Some("#FF0000".into()),
                        background: Some("#000000".into()),
                        options: vec!["BOLD".into()],
                    }],
                },
            )
            .expect("snapshot");
        session.stop().expect("stop");
        let path = session.file_path.clone();
        session.close().expect("close");
        let replay = read_gz_replay(&path);
        assert!(replay.contains("\"columns\":120"));
        assert!(replay.contains("\"rows\":40"));
        assert!(replay.contains("\"pixelWidth\":1600"));
        assert!(replay.contains("\"styleRuns\""));
        assert!(replay.contains("\"foreground\":\"#FF0000\""));
        assert!(replay.contains("\"BOLD\""));
        let _ = fs::remove_dir_all(&dir);
    }

    // ----- Video export -----

    fn styled_frame(
        row: u32,
        column: u32,
        text: &str,
        options: Vec<String>,
        foreground: Option<&str>,
        background: Option<&str>,
    ) -> TerminalRecordingReplayFrame {
        TerminalRecordingReplayFrame {
            content: text.to_string(),
            columns: 0,
            rows: 0,
            pixel_width: 0,
            pixel_height: 0,
            style_runs: vec![TerminalRecordingStyleRun {
                row,
                column,
                text: text.to_string(),
                foreground: foreground.map(ToString::to_string),
                background: background.map(ToString::to_string),
                options,
            }],
            duration_seconds: 1.0,
        }
    }

    #[test]
    fn even_clamps_to_minimum_and_rounds_up() {
        assert_eq!(even(0), 2);
        assert_eq!(even(1), 2);
        assert_eq!(even(2), 2);
        assert_eq!(even(3), 4);
        assert_eq!(even(801), 802);
        assert_eq!(even(802), 802);
    }

    #[test]
    fn render_layout_uses_pixel_dimensions_clamped_to_even() {
        let fonts = ExportFonts::load().expect("fonts");
        let mut frame = ScreenSnapshot::plain("x").to_frame(1.0);
        frame.pixel_width = 801;
        frame.pixel_height = 601;
        let layout = resolve_render_layout(&[frame], &fonts);
        assert_eq!(layout.width, 802);
        assert_eq!(layout.height, 602);
        assert!(layout.cell_width >= 1);
        assert!(layout.line_height >= 1);
        assert!(layout.ascent >= 1);
    }

    #[test]
    fn render_layout_accounts_for_content_and_style_run_extents() {
        let fonts = ExportFonts::load().expect("fonts");
        let mut frame = ScreenSnapshot::plain("ab\ncdef").to_frame(1.0);
        frame.style_runs = vec![TerminalRecordingStyleRun {
            row: 4,
            column: 10,
            text: "xyz".into(),
            foreground: None,
            background: None,
            options: Vec::new(),
        }];
        let layout = resolve_render_layout(&[frame], &fonts);
        // max columns: max(1, "cdef".len()=4, 10+3=13) = 13; max rows: max(2 lines, 4+1) = 5.
        assert_eq!(
            layout.width,
            even(FRAME_PADDING_X * 2 + layout.cell_width * 13)
        );
        assert_eq!(
            layout.height,
            even(FRAME_PADDING_Y * 2 + layout.line_height * 5)
        );
    }

    #[test]
    fn collect_frame_extents_uses_max_over_all_frames() {
        let mut first = ScreenSnapshot::plain("one").to_frame(1.0);
        first.columns = 80;
        first.rows = 24;
        let mut second = ScreenSnapshot::plain("two").to_frame(1.0);
        second.pixel_width = 1600;
        second.pixel_height = 900;
        let extents = collect_frame_extents(&[first, second]);
        assert_eq!(extents.max_columns, 80);
        assert_eq!(extents.max_rows, 24);
        assert_eq!(extents.max_pixel_width, 1600);
        assert_eq!(extents.max_pixel_height, 900);
    }

    #[test]
    fn concat_file_quotes_paths_and_repeats_last_frame() {
        let frames = vec![
            ScreenSnapshot::plain("one").to_frame(0.5),
            ScreenSnapshot::plain("two").to_frame(0.000_1),
        ];
        let files = vec![
            PathBuf::from("/tmp/it's here/frame-00001.png"),
            PathBuf::from("/tmp/it's here/frame-00002.png"),
        ];
        let content = build_concat_file_content(&files, &frames);
        assert_eq!(
            content,
            concat!(
                "file '/tmp/it'\\''s here/frame-00001.png'\n",
                "duration 0.500000\n",
                "file '/tmp/it'\\''s here/frame-00002.png'\n",
                "duration 0.001000\n",
                "file '/tmp/it'\\''s here/frame-00002.png'\n",
            )
        );
    }

    #[test]
    fn parses_ffmpeg_out_time_values() {
        assert_eq!(parse_ffmpeg_out_time_seconds("00:01:05.50"), Some(65.5));
        assert_eq!(
            parse_ffmpeg_out_time_seconds(" 01:00:00.000000 "),
            Some(3600.0)
        );
        assert_eq!(parse_ffmpeg_out_time_seconds("00:00:00"), Some(0.0));
        assert_eq!(parse_ffmpeg_out_time_seconds("-577014:32:22.775808"), None);
        assert_eq!(parse_ffmpeg_out_time_seconds("00:01"), None);
        assert_eq!(parse_ffmpeg_out_time_seconds("bogus"), None);
        assert_eq!(parse_ffmpeg_out_time_seconds(""), None);
    }

    #[test]
    fn ffmpeg_progress_lines_map_to_encoding_fraction() {
        let reported: Arc<Mutex<Vec<(TerminalRecordingExportPhase, f64)>>> =
            Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&reported);
        let listener: ExportProgressListener =
            Box::new(move |phase, fraction| sink.lock().unwrap().push((phase, fraction)));
        handle_ffmpeg_progress_line("out_time=00:00:05.00", 10.0, Some(&listener));
        handle_ffmpeg_progress_line("frame=12", 10.0, Some(&listener));
        handle_ffmpeg_progress_line("out_time=00:00:20.00", 10.0, Some(&listener));
        let reported = reported.lock().unwrap();
        assert_eq!(reported.len(), 2);
        assert_eq!(reported[0].0, TerminalRecordingExportPhase::Encoding);
        assert!((reported[0].1 - (0.70 + 0.5 * 0.29)).abs() < 1e-9);
        // Encoded time beyond the total duration is clamped to 0.99.
        assert!((reported[1].1 - 0.99).abs() < 1e-9);
    }

    #[test]
    fn parse_color_supports_hex_with_fallback() {
        assert_eq!(parse_color(Some("#FF8000"), [1, 2, 3]), [255, 128, 0]);
        assert_eq!(parse_color(Some("#ff8000"), [1, 2, 3]), [255, 128, 0]);
        assert_eq!(parse_color(Some("0x00FF00"), [1, 2, 3]), [0, 255, 0]);
        assert_eq!(parse_color(Some("255"), [1, 2, 3]), [0, 0, 255]);
        assert_eq!(parse_color(Some("garbage"), [1, 2, 3]), [1, 2, 3]);
        assert_eq!(parse_color(Some(""), [1, 2, 3]), [1, 2, 3]);
        assert_eq!(parse_color(None, [1, 2, 3]), [1, 2, 3]);
    }

    #[test]
    fn bounded_process_output_truncates_at_limit() {
        let mut output = BoundedProcessOutput::new();
        let long_line = "x".repeat(MAX_PROCESS_OUTPUT_BYTES);
        output.append_line(&long_line);
        output.append_line("more");
        let text = output.as_string();
        assert!(text.contains("[output truncated after"));
        assert!(text.len() <= MAX_PROCESS_OUTPUT_BYTES + 64);
    }

    #[test]
    fn plain_frame_renders_glyphs_on_black_background() {
        let fonts = ExportFonts::load().expect("fonts");
        let frame = ScreenSnapshot::plain("X").to_frame(1.0);
        let layout = resolve_render_layout(std::slice::from_ref(&frame), &fonts);
        let image = render_frame_image(&frame, &layout, true, &fonts);
        assert_eq!(image.pixel(0, 0), [0, 0, 0]);
        let mut lit = 0;
        for y in FRAME_PADDING_Y..FRAME_PADDING_Y + layout.line_height {
            for x in FRAME_PADDING_X..FRAME_PADDING_X + layout.cell_width {
                if image.pixel(x, y) != [0, 0, 0] {
                    lit += 1;
                }
            }
        }
        assert!(lit > 0, "expected rasterized glyph pixels");
    }

    #[test]
    fn styled_frame_swaps_colors_for_inverse_runs() {
        let fonts = ExportFonts::load().expect("fonts");
        let frame = styled_frame(
            0,
            0,
            "ab",
            vec!["INVERSE".into()],
            Some("#FF0000"),
            Some("#0000FF"),
        );
        let layout = resolve_render_layout(std::slice::from_ref(&frame), &fonts);
        let image = render_frame_image(&frame, &layout, true, &fonts);
        // INVERSE: the cell background becomes the foreground color (#FF0000).
        assert_eq!(image.pixel(FRAME_PADDING_X, FRAME_PADDING_Y), [255, 0, 0]);
    }

    #[test]
    fn styled_frame_skips_text_for_hidden_runs() {
        let fonts = ExportFonts::load().expect("fonts");
        let frame = styled_frame(
            0,
            0,
            "ab",
            vec!["HIDDEN".into()],
            Some("#FF0000"),
            Some("#0000FF"),
        );
        let layout = resolve_render_layout(std::slice::from_ref(&frame), &fonts);
        let image = render_frame_image(&frame, &layout, true, &fonts);
        for y in FRAME_PADDING_Y..FRAME_PADDING_Y + layout.line_height {
            for x in FRAME_PADDING_X..FRAME_PADDING_X + layout.cell_width * 2 {
                assert_eq!(
                    image.pixel(x, y),
                    [0, 0, 255],
                    "hidden text must not render"
                );
            }
        }
    }

    #[test]
    fn styled_frame_without_color_flag_renders_plain() {
        let fonts = ExportFonts::load().expect("fonts");
        let frame = styled_frame(0, 0, "ab", Vec::new(), Some("#FF0000"), Some("#0000FF"));
        let layout = resolve_render_layout(std::slice::from_ref(&frame), &fonts);
        let image = render_frame_image(&frame, &layout, false, &fonts);
        // include_color=false: no background rectangle is painted.
        assert_eq!(image.pixel(FRAME_PADDING_X, FRAME_PADDING_Y), [0, 0, 0]);
    }

    #[test]
    fn ffmpeg_availability_reports_missing_executable() {
        let availability = ffmpeg_availability(Some("/nonexistent/path/to/ffmpeg-kortty-test"));
        assert!(!availability.available);
        assert!(availability.resolved_path.is_none());
    }

    #[test]
    fn whole_tab_recording_records_input_from_any_split() {
        let dir = temp_dir("kortty-wholetab-input");
        let settings = GlobalSettings {
            terminal_recording_directory: Some(dir.to_string_lossy().to_string()),
            terminal_recording_idle_auto_pause: false,
            ..GlobalSettings::default()
        };

        let store = TerminalRecordingStore::new();
        let response = store
            .start(
                TerminalRecordingStartRequest {
                    tab_id: "tab-1".into(),
                    split_id: None,
                    connection_name: Some("prod".into()),
                    scope: TerminalRecordingScope::WholeTab,
                    columns: 80,
                    rows: 24,
                },
                &settings,
                None,
            )
            .expect("start whole-tab recording");
        let replay_path = PathBuf::from(&response.file_path);

        // A second split of the tab announces itself via a screen snapshot whose
        // widget is the split's SSH session id (which is not the tab id).
        store
            .append_snapshot(TerminalRecordingAppendSnapshotRequest {
                session_id: response.session_id.clone(),
                at_millis: Some(now_millis()),
                text: "hello".into(),
                columns: 80,
                rows: 24,
                cursor_column: None,
                cursor_row: None,
                widget: Some("split-b".into()),
                pixel_width: None,
                pixel_height: None,
                style_runs: Vec::new(),
            })
            .expect("append snapshot for split-b");

        // Input from that split must reach the whole-tab recording even though the
        // recording session only stores the tab id, not the split id.
        store.record_input_activity_for_terminal("split-b");
        // The primary terminal id (the tab itself for an unsplit start) routes too.
        store.record_input_activity_for_terminal("tab-1");

        let summary = store.stop(&response.session_id).expect("stop");
        assert_eq!(summary.file_path, response.file_path);

        let replay = read_gz_replay(&replay_path);
        assert_eq!(
            count_occurrences(&replay, "\"type\":\"user_input_activity\""),
            2,
            "input from a split and the tab must both be recorded"
        );

        // After stop the mapping is cleared so a reused split id does not route to
        // a closed session (the call must be a harmless no-op).
        store.record_input_activity_for_terminal("split-b");

        let _ = fs::remove_dir_all(&dir);
    }
}
