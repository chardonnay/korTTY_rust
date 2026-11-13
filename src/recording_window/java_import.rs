//! Import Java korTTY JSONL recordings into native replay without executing their text.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use async_channel::Receiver;
use flate2::bufread::MultiGzDecoder;
use kortty_recording::{CaptureOptions, RecordedCell, RecordedScreen, Recorder, Replay, State};
use kortty_terminal::{Color, Cursor, CursorShape, Style, character_cell_width};
use serde::Deserialize;

use crate::private_files::{Directory, FileStamp};

use super::catalog;

const LEGACY: &str = ".korttyrec.jsonl";
const COMPRESSED: &str = ".korttyrec.jsonl.gz";
const MAX_BYTES: u64 = 512 * 1024 * 1024;
const MAX_LINE: usize = 16 * 1024 * 1024;
const MAX_EVENTS: usize = 1_000_000;
const MAX_WIDGETS: usize = 128;
const MAX_WIDGET_BYTES: usize = 256;
const MAX_COLUMNS: usize = 1024;
const MAX_ROWS: usize = 512;
const MAX_GRID_CELLS: usize = 100_000_000;
const DWC: char = '\u{e000}';

#[derive(Clone, Debug)]
pub struct Imported {
    pub(super) path: PathBuf,
    pub(super) frames: usize,
    pub(super) sync_failed: bool,
}

pub(super) struct Pending {
    pub(super) reply: Receiver<Result<Imported, String>>,
    pub(super) done: Receiver<()>,
    pub(super) progress: Receiver<usize>,
    pub(super) cancel: Cancel,
}

pub(super) struct Cancel(Arc<AtomicBool>);

impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

fn format(path: &Path) -> Option<(&str, bool)> {
    let name = path.file_name()?.to_str()?;
    let lower = name.to_ascii_lowercase();
    let extension = if lower.ends_with(COMPRESSED) {
        COMPRESSED
    } else if lower.ends_with(LEGACY) {
        LEGACY
    } else {
        return None;
    };
    Some((
        &name[..name.len() - extension.len()],
        extension == COMPRESSED,
    ))
}

pub(super) fn supports(path: &Path) -> bool {
    format(path).is_some()
}

pub(super) fn start(source: PathBuf, destination: PathBuf) -> io::Result<Pending> {
    let (send, reply) = async_channel::bounded(1);
    let (finished, done) = async_channel::bounded(1);
    let (report, progress) = async_channel::bounded(1);
    let canceled = Arc::new(AtomicBool::new(false));
    let cancel = Cancel(Arc::clone(&canceled));
    std::thread::Builder::new()
        .name("kortty-java-recording-import".into())
        .spawn(move || {
            let result = import(&source, &destination, &canceled, |frames| {
                let _ = report.try_send(frames);
            });
            let result = result.map_err(|error| {
                if canceled.load(Ordering::Acquire) {
                    "Java recording import canceled.".into()
                } else {
                    format!("Could not import the Java recording ({error}).")
                }
            });
            let _ = send.try_send(result);
            drop(finished);
        })?;
    Ok(Pending {
        reply,
        done,
        progress,
        cancel,
    })
}

fn import(
    source: &Path,
    destination: &Path,
    canceled: &AtomicBool,
    progress: impl FnMut(usize),
) -> io::Result<Imported> {
    active(canceled)?;
    let (stem, compressed) = format(source)
        .ok_or_else(|| invalid("choose a .korttyrec.jsonl or .korttyrec.jsonl.gz recording"))?;
    let name = catalog::checked_name(stem)?;
    let input = catalog::open_regular(source)?;
    let stamp = FileStamp::read(&input)?;
    if !(1..=MAX_BYTES).contains(&input.metadata()?.len()) {
        return Err(invalid(
            "source recording exceeds the 512 MiB size limit or is empty",
        ));
    }
    let directory = Directory::open(destination, true)?;
    if directory.has_entry(&name)? {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "a native recording with this name already exists",
        ));
    }
    let raw = BufReader::new(
        CancelRead {
            source: &input,
            canceled,
        }
        .take(MAX_BYTES + 1),
    );
    let reader: Box<dyn BufRead> = if compressed {
        Box::new(BufReader::new(MultiGzDecoder::new(raw)))
    } else {
        Box::new(raw)
    };
    let mut frames = 0;
    directory.publish_checked_readable(
        &name,
        |file| {
            frames = convert(reader, BufWriter::new(&mut *file), canceled, progress)?;
            active(canceled)?;
            let replay = Replay::from_reader(BufReader::new(CancelRead {
                source: file,
                canceled,
            }))
            .map_err(io::Error::other)?;
            if !replay.is_complete() || replay.frame_count() != frames {
                return Err(invalid("converted recording failed native verification"));
            }
            Ok(())
        },
        || {
            stamp.require_unchanged(&input)?;
            stamp.require_unchanged(&catalog::open_regular(source)?)?;
            if !directory.same_as(&Directory::open(destination, false)?)? {
                return Err(invalid("the recording directory changed during import"));
            }
            active(canceled)
        },
    )?;
    Ok(Imported {
        path: destination.join(name),
        frames,
        sync_failed: directory.sync().is_err(),
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Event {
    #[serde(rename = "type")]
    kind: String,
    at: String,
    format_version: Option<u32>,
    widget: Option<String>,
    content: Option<String>,
    #[serde(default)]
    columns: u16,
    #[serde(default)]
    rows: u16,
    #[serde(default)]
    pixel_width: u16,
    #[serde(default)]
    pixel_height: u16,
    #[serde(default)]
    style_runs: Vec<Run>,
}

#[derive(Deserialize)]
struct Run {
    row: u16,
    column: u16,
    text: String,
    foreground: Option<String>,
    background: Option<String>,
    #[serde(default)]
    options: Vec<String>,
}

pub(super) fn convert(
    mut reader: impl BufRead,
    writer: impl Write,
    canceled: &AtomicBool,
    mut progress: impl FnMut(usize),
) -> io::Result<usize> {
    let mut recorder = Recorder::new(
        writer,
        CaptureOptions {
            include_style: true,
            idle_after: None,
        },
    )
    .map_err(io::Error::other)?;
    let mut widgets = HashMap::new();
    let mut line = Vec::new();
    let mut total = 0;
    let mut lines = 0;
    let mut cells = 0;
    let mut frames = 0;
    let mut reported = Instant::now();
    let mut elapsed = 0u64;
    let mut last_at: Option<i128> = None;
    loop {
        active(canceled)?;
        line.clear();
        let length = reader
            .by_ref()
            .take(MAX_LINE as u64 + 1)
            .read_until(b'\n', &mut line)?;
        if length == 0 {
            break;
        }
        total += length as u64;
        lines += 1;
        if length > MAX_LINE || total > MAX_BYTES || lines > MAX_EVENTS {
            return Err(invalid(
                "recording exceeds the expanded byte, line or event limit",
            ));
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let event: Event = serde_json::from_slice(&line).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid JSONL event at line {lines}"),
            )
        })?;
        let time = event
            .at
            .parse::<jiff::Timestamp>()
            .map_err(|_| invalid("invalid Java recording timestamp"))?
            .as_nanosecond();
        if let Some(previous) = last_at {
            // Java's replay adds positive intervals only, including after wall-clock setbacks.
            let delta = u64::try_from((time - previous).max(0))
                .map_err(|_| invalid("recording timestamp interval is out of range"))?;
            elapsed = elapsed
                .checked_add(delta)
                .ok_or_else(|| invalid("recording timestamp is out of range"))?;
        }
        last_at = Some(time);
        let at = Duration::from_nanos(elapsed);
        match event.kind.as_str() {
            "session_created" if event.format_version.is_some_and(|version| version != 1) => {
                return Err(invalid("unsupported Java recording format version"));
            }
            "recording_start" => recorder.start(at).map_err(io::Error::other)?,
            "recording_stop" | "session_closed" => recorder.stop(at).map_err(io::Error::other)?,
            "auto_pause" => recorder.pause(at).map_err(io::Error::other)?,
            "auto_resume" => recorder.resume(at).map_err(io::Error::other)?,
            "user_input_activity" if recorder.state() == State::Recording => {
                recorder.input_activity(at).map_err(io::Error::other)?;
            }
            "screen" if matches!(recorder.state(), State::Recording | State::AutoPaused) => {
                let widget = event
                    .widget
                    .as_deref()
                    .filter(|id| !id.trim().is_empty())
                    .unwrap_or("terminal");
                if widget.len() > MAX_WIDGET_BYTES {
                    return Err(invalid(
                        "recording widget identifier exceeds its size limit",
                    ));
                }
                let next_pane = widgets.len() as u64 + 1;
                let pane = *widgets.entry(widget.to_owned()).or_insert(next_pane);
                if widgets.len() > MAX_WIDGETS {
                    return Err(invalid("recording exceeds 128 terminal panes"));
                }
                let screen = screen(&event, &mut cells, canceled)?;
                let paused = recorder.state() == State::AutoPaused;
                if paused {
                    // The Java writer normally writes auto_resume first. Older frames lacking
                    // it still display at the current paused playback position.
                    recorder.resume(at).map_err(io::Error::other)?;
                }
                frames += usize::from(
                    recorder
                        .record_imported_screen(at, pane, &screen)
                        .map_err(io::Error::other)?,
                );
                if paused {
                    recorder.pause(at).map_err(io::Error::other)?;
                }
                if reported.elapsed() >= Duration::from_millis(150) {
                    progress(frames);
                    reported = Instant::now();
                }
            }
            _ => {}
        }
    }
    if frames == 0 {
        return Err(invalid("recording contains no terminal screens"));
    }
    active(canceled)?;
    recorder
        .finish(Duration::from_nanos(elapsed))
        .map_err(io::Error::other)?;
    Ok(frames)
}

fn screen(event: &Event, cells: &mut usize, canceled: &AtomicBool) -> io::Result<RecordedScreen> {
    let content = event
        .content
        .as_deref()
        .ok_or_else(|| invalid("screen event is missing its text"))?;
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    let lines = normalized.strip_suffix('\n').unwrap_or(&normalized);
    let lines = lines.split('\n');
    let (mut columns, mut rows) = (usize::from(event.columns), usize::from(event.rows));
    if rows == 0 {
        rows = lines.clone().count().max(
            event
                .style_runs
                .iter()
                .map(|run| usize::from(run.row) + 1)
                .max()
                .unwrap_or(1),
        );
    }
    if columns == 0 {
        columns = if event.style_runs.is_empty() {
            lines.clone().map(display_width).max().unwrap_or(1)
        } else {
            event
                .style_runs
                .iter()
                .map(|run| usize::from(run.column) + display_width(&run.text))
                .max()
                .unwrap_or(1)
        }
        .max(1);
    }
    if columns > MAX_COLUMNS || rows > MAX_ROWS {
        return Err(invalid("screen dimensions exceed 1024 columns or 512 rows"));
    }
    *cells += columns * rows;
    if *cells > MAX_GRID_CELLS {
        return Err(invalid("recording exceeds the decoded grid-cell limit"));
    }
    let mut grid = vec![cell(String::new(), 1, Style::default()); columns * rows];
    if event.style_runs.is_empty() {
        for (row, line) in lines.enumerate() {
            active(canceled)?;
            if row >= rows {
                return Err(invalid("screen text exceeds its declared row count"));
            }
            place(
                &mut grid[row * columns..(row + 1) * columns],
                0,
                line,
                Style::default(),
            )?;
        }
    } else {
        let mut next_column = vec![0; rows];
        for run in &event.style_runs {
            active(canceled)?;
            let row = usize::from(run.row);
            let column = usize::from(run.column);
            if row >= rows || column >= columns || column < next_column[row] {
                return Err(invalid("screen style runs overlap or exceed their grid"));
            }
            next_column[row] = place(
                &mut grid[row * columns..(row + 1) * columns],
                column,
                &run.text,
                style(run),
            )?;
        }
    }
    Ok(RecordedScreen {
        columns: columns as u16,
        rows: rows as u16,
        pixels: (event.pixel_width, event.pixel_height),
        cursor: Cursor::default(),
        cursor_shape: CursorShape::default(),
        cursor_blinking: false,
        reverse_video: false,
        cells: grid,
    })
}

fn glyphs(text: &str) -> impl Iterator<Item = (char, u8)> + '_ {
    let mut chars = text.chars().peekable();
    std::iter::from_fn(move || {
        let ch = chars.next()?;
        let ch = if ch.is_control() || ch == DWC {
            ' '
        } else {
            ch
        };
        let width = if ch != ' ' && chars.peek() == Some(&DWC) {
            chars.next();
            2
        } else {
            character_cell_width(ch).unwrap_or(1) as u8
        };
        Some((ch, width))
    })
}

fn display_width(text: &str) -> usize {
    glyphs(text).map(|(_, width)| usize::from(width)).sum()
}

fn place(
    row: &mut [RecordedCell],
    mut column: usize,
    text: &str,
    style: Style,
) -> io::Result<usize> {
    let start = column;
    for (ch, width) in glyphs(text) {
        if width == 0 {
            if !style.hidden && column > start {
                let previous = column - 1;
                let lead = previous - usize::from(row[previous].width == 0);
                let glyph = &mut row[lead].text;
                if glyph.len() + ch.len_utf8() > 1024 {
                    return Err(invalid("screen glyph exceeds its combining-mark limit"));
                }
                glyph.push(ch);
            }
            continue;
        }
        let end = column + usize::from(width);
        if end > row.len() {
            // Java pads getScreenLines to its UTF-16 width, which can leave extra blanks
            // after a wide character in an old text-only recording.
            if ch == ' ' {
                continue;
            }
            return Err(invalid("screen text exceeds its declared column count"));
        }
        let text = if style.hidden || ch == ' ' {
            String::new()
        } else {
            ch.to_string()
        };
        row[column] = cell(text, width, style);
        if width == 2 {
            row[column + 1] = cell(String::new(), 0, style);
        }
        column = end;
    }
    Ok(column)
}

fn cell(text: String, width: u8, style: Style) -> RecordedCell {
    RecordedCell { text, width, style }
}

fn style(run: &Run) -> Style {
    let has = |value| run.options.iter().any(|option| option == value);
    Style {
        foreground: color(run.foreground.as_deref()),
        background: color(run.background.as_deref()),
        bold: has("BOLD"),
        italic: has("ITALIC"),
        underline: has("UNDERLINED"),
        inverse: has("INVERSE"),
        dim: has("DIM"),
        blink: has("SLOW_BLINK") || has("RAPID_BLINK"),
        hidden: has("HIDDEN"),
        strikethrough: has("STRIKETHROUGH"),
    }
}

fn color(value: Option<&str>) -> Color {
    let Some(hex) = value
        .and_then(|value| value.strip_prefix('#'))
        .filter(|hex| hex.len() == 6)
    else {
        return Color::Default;
    };
    u32::from_str_radix(hex, 16).map_or(Color::Default, |value| {
        let [_, red, green, blue] = value.to_be_bytes();
        Color::Rgb(red, green, blue)
    })
}

struct CancelRead<'a, R> {
    source: R,
    canceled: &'a AtomicBool,
}

impl<R: Read> Read for CancelRead<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        active(self.canceled)?;
        self.source.read(buf)
    }
}

impl<R: Seek> Seek for CancelRead<'_, R> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        active(self.canceled)?;
        self.source.seek(position)
    }
}

fn active(canceled: &AtomicBool) -> io::Result<()> {
    if canceled.load(Ordering::Acquire) {
        return Err(io::Error::other("Java recording import canceled"));
    }
    Ok(())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Cursor as Input;

    use super::super::tests::TestDirectory;

    // Matches the pinned Java TerminalRecordingServiceTest's text-only replay events.
    const LEGACY_TIMING: &str = r#"{"type":"session_created","at":"2026-05-13T12:00:00Z","connection":"prod","formatVersion":1}
{"type":"recording_start","at":"2026-05-13T12:00:00Z","scope":"ACTIVE_SPLIT","segment":1}
{"type":"screen","at":"2026-05-13T12:00:00Z","widget":"split-1","content":"one"}
{"type":"screen","at":"2026-05-13T12:00:00.100Z","widget":"split-1","content":"two"}
{"type":"auto_pause","at":"2026-05-13T12:00:01.100Z","idleMillis":1000}
{"type":"auto_resume","at":"2026-05-13T12:01:01.100Z","source":"screen"}
{"type":"screen","at":"2026-05-13T12:01:01.100Z","widget":"split-1","content":"three"}
{"type":"recording_stop","at":"2026-05-13T12:01:01.600Z","segment":1}
"#;

    const STYLED: &str = r##"{"type":"session_created","at":"2026-05-13T12:00:00Z","formatVersion":1}
{"type":"recording_start","at":"2026-05-13T12:00:00Z","scope":"WHOLE_TAB","segment":1}
{"type":"screen","at":"2026-05-13T12:00:00.000000001Z","widget":"left","content":"界\uE000Asecret\n        \n","columns":10,"rows":2,"pixelWidth":100,"pixelHeight":40,"styleRuns":[{"row":0,"column":0,"text":"界\uE000","foreground":"#FF0000","background":"#001122","options":["BOLD","ITALIC","UNDERLINED","INVERSE","SLOW_BLINK"]},{"row":0,"column":2,"text":"A"},{"row":0,"column":3,"text":"secret","options":["HIDDEN"]}]}
{"type":"screen","at":"2026-05-13T12:00:00.250Z","widget":"right","content":"e\u0301\r\n"}
{"type":"recording_stop","at":"2026-05-13T12:00:01Z"}
{"type":"session_closed","at":"2026-05-13T12:00:09Z"}
"##;

    fn gzip(source: &str) -> Vec<u8> {
        let mut output = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        output.write_all(source.as_bytes()).unwrap();
        output.finish().unwrap()
    }

    #[test]
    fn legacy_java_replay_preserves_elapsed_and_active_time() {
        let mut bytes = Vec::new();
        let count = convert(
            LEGACY_TIMING.as_bytes(),
            &mut bytes,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert_eq!(count, 3);
        let mut replay = Replay::from_reader(Input::new(bytes)).unwrap();
        assert!(replay.is_complete());
        assert_eq!(replay.duration(), Duration::from_millis(1600));
        let last = replay.frame(2).unwrap().unwrap();
        assert_eq!(last.recorded_at, Duration::from_millis(61_100));
        assert_eq!(last.playback_at, Duration::from_millis(1100));
        assert_eq!(last.screen.columns, 5);
        assert_eq!(last.screen.rows, 1);
        assert_eq!(
            last.screen
                .cells
                .iter()
                .map(|cell| cell.text.as_str())
                .collect::<String>(),
            "three"
        );
    }

    #[test]
    fn gzip_java_import_preserves_native_panes_styles_wide_cells_and_source() {
        let directory = TestDirectory::new();
        let base = directory.0.canonicalize().unwrap();
        let source = base.join("java.korttyrec.jsonl.gz");
        let destination = base.join("recordings");
        let input = gzip(STYLED);
        fs::write(&source, &input).unwrap();
        let imported = import(&source, &destination, &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!(imported.path, destination.join("java.korttyrec"));
        assert_eq!(imported.frames, 2);
        assert_eq!(fs::read(&source).unwrap(), input);
        let bytes = fs::read(&imported.path).unwrap();
        assert!(!bytes.windows(6).any(|window| window == b"secret"));
        let mut replay = Replay::from_reader(Input::new(bytes.clone())).unwrap();
        assert!(replay.is_complete());
        let first = replay.frame(0).unwrap().unwrap();
        let second = replay.frame(1).unwrap().unwrap();
        assert_ne!(first.pane, second.pane);
        assert_eq!(first.recorded_at, Duration::from_nanos(1));
        assert_eq!(second.playback_at, Duration::from_nanos(249_999_999));
        assert_eq!(
            (first.screen.columns, first.screen.rows, first.screen.pixels),
            (10, 2, (100, 40))
        );
        let row = first.screen.row(0).unwrap();
        assert_eq!(
            (row[0].text.as_str(), row[0].width, row[1].width),
            ("界", 2, 0)
        );
        assert_eq!(row[0].style.foreground, Color::Rgb(255, 0, 0));
        assert_eq!(row[0].style.background, Color::Rgb(0, 17, 34));
        assert!(
            row[0].style.bold
                && row[0].style.italic
                && row[0].style.underline
                && row[0].style.inverse
                && row[0].style.blink
        );
        assert!(
            row[3..9]
                .iter()
                .all(|cell| cell.style.hidden && cell.text.is_empty())
        );
        assert_eq!(second.screen.row(0).unwrap()[0].text, "e\u{301}");
        assert!(import(&source, &destination, &AtomicBool::new(false), |_| {}).is_err());
        assert_eq!(fs::read(&imported.path).unwrap(), bytes);
    }

    #[test]
    #[ignore = "requires KORTTY_JAVA_RECORDING_FIXTURE from the pinned Java writer"]
    fn pinned_java_writer_recording_imports_styled_panes_and_playback_time() {
        let source = std::env::var_os("KORTTY_JAVA_RECORDING_FIXTURE")
            .map(std::path::PathBuf::from)
            .expect("provide the pinned Java recording fixture");
        let directory = TestDirectory::new();
        let destination = directory.0.canonicalize().unwrap();
        let result = import(&source, &destination, &AtomicBool::new(false), |_| {}).unwrap();
        let mut replay = Replay::open(&result.path).unwrap();
        assert_eq!(result.frames, 2);
        assert!(replay.is_complete());
        assert_eq!(replay.duration(), Duration::from_secs(1));
        let left = replay.frame(0).unwrap().unwrap();
        let right = replay.frame(1).unwrap().unwrap();
        assert_ne!(left.pane, right.pane);
        assert_eq!(left.recorded_at, Duration::from_millis(1));
        assert_eq!(right.playback_at, Duration::from_millis(250));
        assert_eq!(
            (left.screen.columns, left.screen.rows, left.screen.pixels),
            (10, 2, (100, 40))
        );
        let row = left.screen.row(0).unwrap();
        assert_eq!(
            (row[0].text.as_str(), row[0].width, row[1].width),
            ("界", 2, 0)
        );
        assert!(row[0].style.bold);
        assert_eq!(row[0].style.foreground, Color::Rgb(255, 0, 0));
        assert!(
            row[3..9]
                .iter()
                .all(|cell| cell.style.hidden && cell.text.is_empty())
        );
        assert_eq!(right.screen.row(0).unwrap()[0].text, "e\u{301}");
    }

    #[test]
    fn corrupt_oversized_grid_and_canceled_imports_leave_no_native_file() {
        let directory = TestDirectory::new();
        let base = directory.0.canonicalize().unwrap();
        let destination = base.join("recordings");
        let source = base.join("java.korttyrec.jsonl.gz");
        let mut corrupt = gzip(STYLED);
        corrupt.pop();
        fs::write(&source, corrupt).unwrap();
        assert!(import(&source, &destination, &AtomicBool::new(false), |_| {}).is_err());
        fs::write(
            &source,
            gzip(&STYLED.replace("\"columns\":10", "\"columns\":1025")),
        )
        .unwrap();
        assert!(import(&source, &destination, &AtomicBool::new(false), |_| {}).is_err());
        fs::write(&source, gzip(STYLED)).unwrap();
        assert!(import(&source, &destination, &AtomicBool::new(true), |_| {}).is_err());
        assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
    }
}
