use crate::model::terminal_recording::{
    TerminalRecordingAppendInputRequest, TerminalRecordingAppendSnapshotRequest,
    TerminalRecordingExportFormat, TerminalRecordingReplayEvent, TerminalRecordingReplayFile,
    TerminalRecordingReplaySummary, TerminalRecordingStartRequest, TerminalRecordingStartResponse,
    TerminalRecordingState, TerminalRecordingToolAvailability, TerminalRecordingVideoExportOptions,
};
use crate::persistence::xml_repository;
use anyhow::{anyhow, Context, Result};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

const RECORDING_VERSION: u8 = 2;
const DEFAULT_WIDTH: u32 = 1280;
const DEFAULT_HEIGHT: u32 = 720;
const DEFAULT_FPS: u32 = 12;

pub struct TerminalRecordingStore {
    sessions: Mutex<HashMap<String, TerminalRecordingSession>>,
}

struct TerminalRecordingSession {
    file_path: PathBuf,
    writer: GzEncoder<File>,
    started_at_millis: i64,
    paused: bool,
}

impl TerminalRecordingStore {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }

    pub fn start(
        &self,
        request: TerminalRecordingStartRequest,
    ) -> Result<TerminalRecordingStartResponse> {
        let session_id = uuid::Uuid::new_v4().to_string();
        let started_at_millis = chrono::Utc::now().timestamp_millis();
        let directory = default_recording_dir()?;
        fs::create_dir_all(&directory)?;
        let name = recording_file_name(
            request.connection_name.as_deref(),
            &request.tab_id,
            request.split_id.as_deref(),
            started_at_millis,
        );
        let file_path = directory.join(name);
        let file = File::create(&file_path)
            .with_context(|| format!("Failed to create {}", file_path.display()))?;
        let mut writer = GzEncoder::new(file, Compression::default());
        write_event(
            &mut writer,
            &json!({
                "eventType": "metadata",
                "version": RECORDING_VERSION,
                "atMillis": started_at_millis,
                "sessionId": session_id,
                "tabId": request.tab_id,
                "splitId": request.split_id,
                "connectionName": request.connection_name,
                "scope": request.scope,
                "columns": request.columns,
                "rows": request.rows
            }),
        )?;
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|error| anyhow!(error.to_string()))?;
        sessions.insert(
            session_id.clone(),
            TerminalRecordingSession {
                file_path: file_path.clone(),
                writer,
                started_at_millis,
                paused: false,
            },
        );
        Ok(TerminalRecordingStartResponse {
            session_id,
            file_path: file_path.to_string_lossy().to_string(),
            state: TerminalRecordingState::Recording,
            started_at_millis,
        })
    }

    pub fn append_snapshot(&self, request: TerminalRecordingAppendSnapshotRequest) -> Result<()> {
        self.with_session_writer(&request.session_id, |session| {
            if session.paused {
                return Ok(());
            }
            write_event(
                &mut session.writer,
                &json!({
                    "eventType": "snapshot",
                    "atMillis": request.at_millis.unwrap_or_else(|| chrono::Utc::now().timestamp_millis()),
                    "text": request.text,
                    "columns": request.columns,
                    "rows": request.rows,
                    "cursorColumn": request.cursor_column,
                    "cursorRow": request.cursor_row
                }),
            )
        })
    }

    pub fn append_input(&self, request: TerminalRecordingAppendInputRequest) -> Result<()> {
        self.with_session_writer(&request.session_id, |session| {
            if session.paused {
                return Ok(());
            }
            write_event(
                &mut session.writer,
                &json!({
                    "eventType": "input",
                    "atMillis": request.at_millis.unwrap_or_else(|| chrono::Utc::now().timestamp_millis()),
                    "text": request.text
                }),
            )
        })
    }

    pub fn pause(&self, session_id: &str) -> Result<()> {
        self.set_paused(session_id, true)
    }

    pub fn resume(&self, session_id: &str) -> Result<()> {
        self.set_paused(session_id, false)
    }

    pub fn stop(&self, session_id: &str) -> Result<TerminalRecordingReplaySummary> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|error| anyhow!(error.to_string()))?;
        let mut session = sessions
            .remove(session_id)
            .ok_or_else(|| anyhow!("Recording session not found"))?;
        let ended_at_millis = chrono::Utc::now().timestamp_millis();
        write_event(
            &mut session.writer,
            &json!({
                "eventType": "stop",
                "atMillis": ended_at_millis,
                "startedAtMillis": session.started_at_millis
            }),
        )?;
        session.writer.finish()?;
        summarize_replay(&session.file_path)
    }

    fn set_paused(&self, session_id: &str, paused: bool) -> Result<()> {
        self.with_session_writer(session_id, |session| {
            session.paused = paused;
            write_event(
                &mut session.writer,
                &json!({
                    "eventType": if paused { "pause" } else { "resume" },
                    "atMillis": chrono::Utc::now().timestamp_millis()
                }),
            )
        })
    }

    fn with_session_writer<F>(&self, session_id: &str, action: F) -> Result<()>
    where
        F: FnOnce(&mut TerminalRecordingSession) -> Result<()>,
    {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|error| anyhow!(error.to_string()))?;
        let session = sessions
            .get_mut(session_id)
            .ok_or_else(|| anyhow!("Recording session not found"))?;
        action(session)?;
        session.writer.flush()?;
        Ok(())
    }
}

impl Default for TerminalRecordingStore {
    fn default() -> Self {
        Self::new()
    }
}

pub fn default_recording_dir() -> Result<PathBuf> {
    Ok(xml_repository::config_dir()?.join("recordings"))
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
    let target_name = format!("{}{}", sanitize_file_segment(name), extension);
    let target = parent.join(target_name);
    fs::rename(&path, &target)?;
    summarize_replay(&target)
}

pub fn ffmpeg_availability() -> TerminalRecordingToolAvailability {
    let path = find_tool("ffmpeg");
    TerminalRecordingToolAvailability {
        available: path.is_some(),
        path: path.map(|value| value.to_string_lossy().to_string()),
    }
}

pub fn export_video(options: TerminalRecordingVideoExportOptions) -> Result<()> {
    let ffmpeg = find_tool("ffmpeg").ok_or_else(|| anyhow!("ffmpeg is not available"))?;
    let replay = load_replay(&options.replay_path)?;
    let snapshots = replay
        .events
        .iter()
        .filter(|event| event.event_type == "snapshot")
        .filter_map(|event| {
            event
                .text
                .as_ref()
                .map(|text| (event.at_millis, text.as_str()))
        })
        .collect::<Vec<_>>();
    if snapshots.is_empty() {
        return Err(anyhow!("Replay contains no terminal snapshots to export"));
    }
    let width = options.width.unwrap_or(DEFAULT_WIDTH).clamp(320, 3840);
    let height = options.height.unwrap_or(DEFAULT_HEIGHT).clamp(240, 2160);
    let fps = options
        .frames_per_second
        .unwrap_or(DEFAULT_FPS)
        .clamp(1, 60);
    let speed = options.speed.unwrap_or(1.0).clamp(0.1, 8.0);
    let target = PathBuf::from(&options.target_path);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    validate_video_extension(&target, &options.format)?;

    let temp_dir = std::env::temp_dir().join(format!(
        "kortty-recording-export-{}",
        uuid::Uuid::new_v4().simple()
    ));
    fs::create_dir_all(&temp_dir)?;
    let frame_count = snapshots.len().max(1);
    for (index, (_, text)) in snapshots.iter().enumerate() {
        let frame_path = temp_dir.join(format!("frame-{index:05}.ppm"));
        write_snapshot_ppm(&frame_path, text, width, height)?;
    }
    let input_pattern = temp_dir.join("frame-%05d.ppm");
    let frame_duration =
        ((snapshots.last().unwrap().0 - snapshots.first().unwrap().0) as f64 / 1000.0 / speed)
            .max(frame_count as f64 / fps as f64);
    let export_fps = ((frame_count as f64 / frame_duration).round() as u32).clamp(1, fps);
    let status = Command::new(ffmpeg)
        .arg("-y")
        .arg("-framerate")
        .arg(export_fps.to_string())
        .arg("-i")
        .arg(&input_pattern)
        .arg("-pix_fmt")
        .arg("yuv420p")
        .arg(&target)
        .status()
        .context("Failed to run ffmpeg")?;
    let _ = fs::remove_dir_all(&temp_dir);
    if !status.success() {
        return Err(anyhow!("ffmpeg export failed with status {status}"));
    }
    Ok(())
}

fn summarize_replay(path: &Path) -> Result<TerminalRecordingReplaySummary> {
    ensure_replay_path(path)?;
    let metadata = fs::metadata(path)?;
    let events = read_replay_events(path)?;
    let started_at_millis = events
        .iter()
        .find(|event| event.event_type == "metadata")
        .map(|event| event.at_millis)
        .or_else(|| events.first().map(|event| event.at_millis));
    let ended_at_millis = events
        .iter()
        .rev()
        .find(|event| matches!(event.event_type.as_str(), "stop" | "snapshot" | "input"))
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
            event_type: string_field(&value, "eventType")
                .or_else(|| string_field(&value, "type"))
                .unwrap_or_else(|| "snapshot".into()),
            at_millis: value
                .get("atMillis")
                .and_then(Value::as_i64)
                .or_else(|| value.get("timestamp").and_then(Value::as_i64))
                .unwrap_or_default(),
            text: string_field(&value, "text").or_else(|| string_field(&value, "data")),
            columns: u16_field(&value, "columns"),
            rows: u16_field(&value, "rows"),
            cursor_column: u16_field(&value, "cursorColumn"),
            cursor_row: u16_field(&value, "cursorRow"),
        });
    }
    Ok(events)
}

fn write_event(writer: &mut impl Write, event: &Value) -> Result<()> {
    serde_json::to_writer(&mut *writer, event)?;
    writer.write_all(b"\n")?;
    Ok(())
}

fn write_snapshot_ppm(path: &Path, text: &str, width: u32, height: u32) -> Result<()> {
    let mut pixels = vec![0x11_u8; (width * height * 3) as usize];
    for chunk in pixels.chunks_exact_mut(3) {
        chunk[0] = 0x11;
        chunk[1] = 0x11;
        chunk[2] = 0x1b;
    }
    let clean = strip_ansi(text);
    let char_width = 8;
    let char_height = 14;
    let margin_x = 24;
    let margin_y = 24;
    for (row, line) in clean.lines().enumerate() {
        let y = margin_y + row as u32 * char_height;
        if y + char_height >= height {
            break;
        }
        for (col, ch) in line.chars().enumerate() {
            if ch.is_whitespace() {
                continue;
            }
            let x = margin_x + col as u32 * char_width;
            if x + char_width >= width {
                break;
            }
            draw_cell(&mut pixels, width, height, x, y, ch);
        }
    }
    let mut file = File::create(path)?;
    writeln!(file, "P6\n{} {}\n255", width, height)?;
    file.write_all(&pixels)?;
    Ok(())
}

fn draw_cell(pixels: &mut [u8], width: u32, height: u32, x: u32, y: u32, ch: char) {
    let color = if ch.is_ascii_digit() {
        [0xf9, 0xe2, 0xaf]
    } else if ch.is_ascii_punctuation() {
        [0x89, 0xb4, 0xfa]
    } else {
        [0xcd, 0xd6, 0xf4]
    };
    for dy in 2..11 {
        for dx in 1..6 {
            let px = x + dx;
            let py = y + dy;
            if px >= width || py >= height {
                continue;
            }
            if (dx == 1 || dx == 5 || dy == 2 || dy == 10) && ch != '_' {
                continue;
            }
            let index = ((py * width + px) * 3) as usize;
            pixels[index..index + 3].copy_from_slice(&color);
        }
    }
}

fn strip_ansi(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\x1b' {
            output.push(ch);
            continue;
        }
        match chars.peek().copied() {
            Some('[') => {
                chars.next();
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                for next in chars.by_ref() {
                    if next == '\x07' {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    output
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

fn recording_file_name(
    connection_name: Option<&str>,
    tab_id: &str,
    split_id: Option<&str>,
    started_at_millis: i64,
) -> String {
    let timestamp = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(started_at_millis)
        .unwrap_or_else(chrono::Utc::now)
        .format("%Y%m%d-%H%M%S");
    let mut base = sanitize_file_segment(
        connection_name
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(tab_id),
    );
    if let Some(split_id) = split_id.filter(|value| !value.trim().is_empty()) {
        base.push('-');
        base.push_str(&sanitize_file_segment(split_id));
    }
    format!("{base}-{timestamp}.korttyrec.jsonl.gz")
}

fn sanitize_file_segment(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    let trimmed = sanitized.trim_matches('-');
    if trimmed.is_empty() {
        "terminal".into()
    } else {
        trimmed.chars().take(80).collect()
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_current_and_legacy_replay_extensions() {
        assert!(is_replay_path(Path::new("a.korttyrec.jsonl")));
        assert!(is_replay_path(Path::new("a.korttyrec.jsonl.gz")));
        assert!(!is_replay_path(Path::new("a.json")));
    }

    #[test]
    fn sanitizes_empty_names_to_terminal() {
        assert_eq!(sanitize_file_segment("///"), "terminal");
        assert_eq!(sanitize_file_segment("prod host"), "prod-host");
    }
}
