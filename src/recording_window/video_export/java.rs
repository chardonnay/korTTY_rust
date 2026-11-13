//! A bounded Java replay excerpt consumable by the pinned korTTY reader.

use std::io::{self, BufReader, Seek, Write};
use std::path::Component;
use std::sync::atomic::AtomicBool;

use flate2::{Compression, bufread::MultiGzDecoder, write::GzEncoder};
use kortty_recording::{RecordedCell, RecordedScreen, ReplayFrame};
use kortty_terminal::{Style, character_cell_width};
use serde::Serialize;

use crate::private_files::Directory;
use crate::recording_window::{catalog, java_import, render};

use super::{Progress, Request, Saved, active, invalid, timing};

pub(super) const SUFFIX: &str = ".korttyrec.jsonl.gz";
const MAX_EXPANDED: usize = 64 * 1024 * 1024;
const MAX_LINE: usize = 16 * 1024 * 1024;
const MAX_CELLS: usize = 100_000_000;
const DWC: char = '\u{e000}';

pub(super) fn export(
    request: &Request,
    canceled: &AtomicBool,
    mut progress: impl FnMut(Progress),
) -> io::Result<Saved> {
    active(canceled)?;
    let path = &request.output;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| {
            name.strip_suffix(SUFFIX)
                .is_some_and(|stem| !stem.is_empty())
                && !name.chars().any(char::is_control)
        });
    let parent = path.parent().filter(|_| {
        path.is_absolute() && !path.components().any(|part| part == Component::ParentDir)
    });
    let (Some(parent), Some(name)) = (parent, name) else {
        return Err(invalid(
            "choose an absolute path ending in .korttyrec.jsonl.gz",
        ));
    };
    let destination = Directory::open(parent, false)?;
    if destination.has_entry(name)? {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "a file already exists at the selected export path",
        ));
    }
    let require_source = || {
        request
            .source_stamp
            .require_unchanged(&catalog::open_regular(&request.source_path)?)
    };
    require_source()?;
    destination.publish_checked_readable(
        name,
        |file| {
            let frames = {
                let mut replay = request
                    .source
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                let timeline = timing::Timeline::new(&replay, request.range.clone(), canceled)?;
                let layout = (replay.pane_count() > 1)
                    .then(|| timing::Layout::grid(&mut replay, &timeline, canceled))
                    .transpose()?;
                let gzip =
                    GzEncoder::new(Limit::new(&mut *file, MAX_EXPANDED), Compression::fast());
                let mut output = Limit::new(gzip, MAX_EXPANDED);
                let frames = write_replay(
                    &mut replay,
                    &timeline,
                    layout.as_ref(),
                    request.include_color,
                    &mut output,
                    canceled,
                    &mut progress,
                )?;
                output.inner.finish()?;
                frames
            };
            active(canceled)?;
            file.rewind()?;
            let gzip = MultiGzDecoder::new(BufReader::new(file));
            // Re-read the exact unpublished stream through the same bounded interchange reader.
            let verified =
                java_import::convert(BufReader::new(gzip), io::sink(), canceled, |_| {})?;
            if verified != frames {
                return Err(invalid("Java recording did not pass replay verification"));
            }
            progress(Progress::Publishing);
            Ok(())
        },
        || {
            require_source()?;
            if !destination.same_as(&Directory::open(parent, false)?)? {
                return Err(invalid("the selected recording output directory changed"));
            }
            active(canceled)
        },
    )?;
    Ok(Saved {
        path: path.clone(),
        sync_failed: destination.sync().is_err(),
    })
}

fn write_replay(
    replay: &mut kortty_recording::Replay,
    timeline: &timing::Timeline,
    layout: Option<&timing::Layout>,
    colors: bool,
    output: &mut impl Write,
    canceled: &AtomicBool,
    mut progress: impl FnMut(Progress),
) -> io::Result<usize> {
    event(
        output,
        &Event::SessionCreated {
            at: timestamp(0)?,
            format_version: 1,
            connection: "Native recording",
        },
    )?;
    event(
        output,
        &Event::RecordingStart {
            at: timestamp(0)?,
            scope: if layout.is_some() {
                "WHOLE_TAB"
            } else {
                "ACTIVE_SPLIT"
            },
            segment: 1,
        },
    )?;
    let mut panes = vec![None; replay.pane_count()];
    let mut previous = None;
    let mut frames = 0;
    let mut cells = 0;
    for (index, sample) in timeline.samples.iter().enumerate() {
        active(canceled)?;
        for (pane, next) in panes.iter_mut().zip(timing::indices(replay, sample.at)) {
            if let Some(next) = next
                && pane.as_ref().is_none_or(|(current, _)| *current != next)
            {
                *pane = Some((next, timing::decode(replay, next)?));
            }
        }
        let frame = Snapshot::build(layout, &panes, colors, canceled)?;
        cells += usize::from(frame.columns) * usize::from(frame.rows);
        if cells > MAX_CELLS {
            return Err(invalid("Java recording exceeds the total grid-cell limit"));
        }
        if previous.as_ref() != Some(&frame) {
            event(
                output,
                &Event::Screen {
                    at: timestamp(sample.millis)?,
                    widget: "terminal",
                    frame: &frame,
                },
            )?;
            previous = Some(frame);
            frames += 1;
        }
        progress(Progress::Writing {
            ready: index + 1,
            total: timeline.samples.len(),
        });
    }
    event(
        output,
        &Event::RecordingStop {
            at: timestamp(timeline.millis)?,
            segment: 1,
        },
    )?;
    event(
        output,
        &Event::SessionClosed {
            at: timestamp(timeline.millis)?,
        },
    )?;
    Ok(frames)
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Event<'a> {
    SessionCreated {
        at: String,
        #[serde(rename = "formatVersion")]
        format_version: u8,
        connection: &'static str,
    },
    RecordingStart {
        at: String,
        scope: &'static str,
        segment: u8,
    },
    Screen {
        at: String,
        widget: &'static str,
        #[serde(flatten)]
        frame: &'a Snapshot,
    },
    RecordingStop {
        at: String,
        segment: u8,
    },
    SessionClosed {
        at: String,
    },
}

#[derive(PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    columns: u16,
    rows: u16,
    pixel_width: u16,
    pixel_height: u16,
    content: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    style_runs: Vec<Run>,
}

#[derive(PartialEq, Eq, Serialize)]
struct Run {
    row: u16,
    column: u16,
    text: String,
    foreground: String,
    background: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    options: Vec<&'static str>,
}

impl Snapshot {
    fn build(
        layout: Option<&timing::Layout>,
        panes: &[Option<(usize, ReplayFrame)>],
        colors: bool,
        canceled: &AtomicBool,
    ) -> io::Result<Self> {
        let first = panes
            .first()
            .and_then(Option::as_ref)
            .map(|(_, frame)| &frame.screen);
        let (columns, rows, pixels) = match (layout, first) {
            (Some(layout), _) => (
                layout.dimensions.0 as u16,
                layout.dimensions.1 as u16,
                (0, 0),
            ),
            (None, Some(screen)) => (screen.columns, screen.rows, screen.pixels),
            (None, None) => {
                return Err(invalid(
                    "no terminal frame is available at the selected time",
                ));
            }
        };
        let mut snapshot = Self {
            columns,
            rows,
            pixel_width: pixels.0,
            pixel_height: pixels.1,
            content: String::new(),
            style_runs: Vec::new(),
        };
        let screens: Vec<_> = panes
            .iter()
            .enumerate()
            .filter_map(|(index, pane)| {
                pane.as_ref().map(|(_, frame)| {
                    (
                        layout.map_or((0, 0), |layout| layout.origins[index]),
                        &frame.screen,
                    )
                })
            })
            .collect();
        let mut cells = vec![None; usize::from(columns)];
        let mut budget = 0;
        for row in 0..rows {
            active(canceled)?;
            cells.fill(None);
            for &((x, y), screen) in &screens {
                fill_row(&mut cells, row, x, y, screen);
            }
            snapshot.append_row(row, &cells, colors, &mut budget)?;
        }
        Ok(snapshot)
    }

    fn append_row(
        &mut self,
        row: u16,
        cells: &[Option<(&RecordedCell, bool)>],
        colors: bool,
        budget: &mut usize,
    ) -> io::Result<()> {
        let mut current: Option<(u16, Style, String)> = None;
        let mut glyph = String::new();
        for (column, cell) in cells.iter().copied().enumerate() {
            if cell.is_some_and(|(cell, _)| cell.width == 0) {
                continue;
            }
            glyph.clear();
            let mut style = Style::default();
            if let Some((cell, reverse)) = cell {
                style = cell.style;
                style.inverse ^= reverse;
                append_glyph(&mut glyph, cell);
            } else {
                glyph.push(' ');
            }
            *budget += glyph.len() * if colors { 2 } else { 1 };
            self.content.push_str(&glyph);
            if colors {
                if current.as_ref().is_some_and(|(_, old, _)| *old != style) {
                    self.push_run(row, current.take().expect("style run exists"));
                }
                if current.is_none() {
                    *budget += 128;
                }
                current
                    .get_or_insert_with(|| (column as u16, style, String::new()))
                    .2
                    .push_str(&glyph);
            }
            if *budget >= MAX_LINE {
                return Err(invalid("Java screen exceeds its bounded JSONL size"));
            }
        }
        if let Some(run) = current {
            self.push_run(row, run);
        }
        self.content.push('\n');
        Ok(())
    }

    fn push_run(&mut self, row: u16, (column, style, text): (u16, Style, String)) {
        let (foreground, background) = render::replay_style_rgb(style);
        let hex = |[r, g, b]: [u8; 3]| format!("#{r:02X}{g:02X}{b:02X}");
        let options = [
            (style.bold, "BOLD"),
            (style.italic, "ITALIC"),
            (style.underline, "UNDERLINED"),
            (style.inverse, "INVERSE"),
            (style.dim, "DIM"),
            (style.blink, "SLOW_BLINK"),
            (style.hidden, "HIDDEN"),
            (style.strikethrough, "STRIKETHROUGH"),
        ]
        .into_iter()
        .filter_map(|(on, name)| on.then_some(name))
        .collect();
        self.style_runs.push(Run {
            row,
            column,
            text,
            foreground: hex(foreground),
            background: hex(background),
            options,
        });
    }
}

fn fill_row<'a>(
    cells: &mut [Option<(&'a RecordedCell, bool)>],
    row: u16,
    x: u32,
    y: u32,
    screen: &'a RecordedScreen,
) {
    let Some(local) = u32::from(row)
        .checked_sub(y)
        .and_then(|row| u16::try_from(row).ok())
        .and_then(|row| screen.row(row))
    else {
        return;
    };
    for (target, cell) in cells[x as usize..].iter_mut().zip(local) {
        *target = Some((cell, screen.reverse_video));
    }
}

fn append_glyph(output: &mut String, cell: &RecordedCell) {
    if cell.style.hidden || cell.text.is_empty() {
        for _ in 0..cell.width {
            output.push(' ');
        }
        return;
    }
    let mut chars = cell.text.chars();
    let lead = chars.next().expect("nonempty glyph");
    let safe = |ch: char| !ch.is_control() && ch != DWC;
    let lead_width = character_cell_width(lead);
    let valid = safe(lead)
        && lead_width.is_some_and(|width| width != 0 && width <= usize::from(cell.width))
        && chars
            .clone()
            .all(|ch| safe(ch) && character_cell_width(ch) == Some(0));
    let lead = if valid { lead } else { '\u{fffd}' };
    output.push(lead);
    if cell.width == 2 && (lead.len_utf16() == 1 || character_cell_width(lead) != Some(2)) {
        output.push(DWC);
    }
    if valid {
        output.extend(chars);
    }
}

fn timestamp(millis: u64) -> io::Result<String> {
    // A native replay stores elapsed time, so use a fixed origin rather than inventing a date.
    jiff::Timestamp::from_nanosecond(i128::from(millis) * 1_000_000)
        .map(|timestamp| timestamp.to_string())
        .map_err(io::Error::other)
}

fn event(output: &mut impl Write, event: &Event<'_>) -> io::Result<()> {
    let mut line = Limit::new(Vec::new(), MAX_LINE - 1);
    serde_json::to_writer(&mut line, event).map_err(io::Error::other)?;
    output.write_all(&line.inner)?;
    output.write_all(b"\n")
}

struct Limit<W> {
    inner: W,
    remaining: usize,
}

impl<W> Limit<W> {
    fn new(inner: W, remaining: usize) -> Self {
        Self { inner, remaining }
    }
}

impl<W: Write> Write for Limit<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.remaining {
            return Err(invalid("Java recording exceeds its byte limit"));
        }
        let count = self.inner.write(bytes)?;
        self.remaining -= count;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::{Cursor, Read};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex, atomic::Ordering};
    use std::time::Duration;

    use iced::futures::{StreamExt, executor::block_on};
    use iced_runtime::{Action, task};
    use kortty_recording::{CaptureOptions, Recorder, Replay};
    use kortty_terminal::{Color, Terminal};

    use crate::private_files::FileStamp;
    use crate::recording_window::{Message, RecordingWindow, tests::TestDirectory};

    use super::super::Format;

    fn capture(path: &Path) {
        let seconds = Duration::from_secs;
        let mut recorder = Recorder::create(
            path,
            CaptureOptions {
                include_style: true,
                idle_after: None,
            },
        )
        .unwrap();
        let mut left = Terminal::new(10, 2);
        let mut right = Terminal::new(8, 1);
        left.advance(
            "\x1b[?5h\x1b[1;4;31;48;2;1;34;80m界e\u{301}\x1b[0m\x1b[8msecret\x1b[0m".as_bytes(),
        );
        right.advance(b"right");
        recorder.start(seconds(0)).unwrap();
        recorder
            .record_screen(seconds(0), 9, &right.snapshot(), (80, 18))
            .unwrap();
        recorder
            .record_screen(seconds(0), 3, &left.snapshot(), (100, 36))
            .unwrap();
        left.advance(b"!");
        recorder
            .record_screen(seconds(2), 3, &left.snapshot(), (100, 36))
            .unwrap();
        recorder.pause(seconds(2)).unwrap();
        recorder.resume(seconds(5)).unwrap();
        right.advance(b"\x1b[2J\x1b[Hnew");
        recorder
            .record_screen(seconds(5), 9, &right.snapshot(), (80, 18))
            .unwrap();
        left.advance(b"\x1b[2J\x1b[Hafter");
        recorder
            .record_screen(seconds(6), 3, &left.snapshot(), (100, 36))
            .unwrap();
        recorder.finish(seconds(8)).unwrap();
    }

    fn request(source: PathBuf, output: PathBuf) -> Request {
        let file = catalog::open_regular(&source).unwrap();
        let source_stamp = FileStamp::read(&file).unwrap();
        Request {
            source: Arc::new(Mutex::new(
                Replay::from_reader(BufReader::new(file)).unwrap(),
            )),
            source_path: source,
            source_stamp,
            output,
            range: Duration::from_secs(1)..Duration::from_secs(3),
            format: Format::Java,
            include_color: true,
            executable: None,
        }
    }

    fn decoded(path: &Path) -> Vec<u8> {
        let input = catalog::open_regular(path).unwrap();
        let mut bytes = Vec::new();
        MultiGzDecoder::new(BufReader::new(input))
            .read_to_end(&mut bytes)
            .unwrap();
        bytes
    }

    #[test]
    fn java_range_export_uses_normal_viewer_without_ffmpeg_and_round_trips_the_composite() {
        let scratch = TestDirectory::new();
        let root = scratch.0.canonicalize().unwrap();
        let source = root.join("source.korttyrec");
        let output = root.join("excerpt.korttyrec.jsonl.gz");
        capture(&source);
        let mut viewer = RecordingWindow::replay(source.clone(), true);
        let mut opening = task::into_stream(viewer.initialize()).unwrap();
        let Some(Action::Output(opened @ Message::Opened(Ok(_)))) = block_on(opening.next()) else {
            panic!("expected an indexed native recording");
        };
        let _ = viewer.update(opened);
        for edit in [
            Message::ExportFormat(Format::Java),
            Message::ExportStart("0:01".into()),
            Message::ExportEnd("0:03".into()),
            Message::ExportPath(output.to_str().unwrap().into()),
        ] {
            let _ = viewer.update(edit);
        }
        let mut outcomes = task::into_stream(viewer.update(Message::Export)).unwrap();
        let done = viewer.take_recording_job_done().unwrap();
        loop {
            let Some(Action::Output(event)) = block_on(outcomes.next()) else {
                panic!("expected a Java recording export result");
            };
            match &event {
                Message::RangeExported(_, Ok(saved)) => {
                    assert_eq!(saved.path, output);
                    let _ = viewer.update(event);
                    break;
                }
                Message::RangeExported(_, Err(error)) => panic!("{error}"),
                _ => {
                    let _ = viewer.update(event);
                }
            }
        }
        assert!(done.recv_blocking().is_err());
        assert!(source.exists());
        let bytes = decoded(&output);
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(!text.contains("secret"));
        assert!(!text.contains("after"));
        assert!(text.contains("界\u{e000}e\u{301}"));
        assert!(text.contains("\"formatVersion\":1"));
        assert!(text.contains("\"scope\":\"WHOLE_TAB\""));
        let mut native = Vec::new();
        assert_eq!(
            java_import::convert(
                bytes.as_slice(),
                &mut native,
                &AtomicBool::new(false),
                |_| {}
            )
            .unwrap(),
            2
        );
        let mut replay = Replay::from_reader(Cursor::new(native)).unwrap();
        assert_eq!(replay.pane_count(), 1);
        assert_eq!(replay.duration(), Duration::from_secs(2));
        assert_eq!(replay.frame_time(1), Some(Duration::from_secs(1)));
        let first = replay.frame(0).unwrap().unwrap().screen;
        assert_eq!((first.columns, first.rows, first.pixels), (19, 2, (0, 0)));
        assert!(!first.cursor.visible);
        let row = first.row(0).unwrap();
        assert_eq!(
            (
                row[0].text.as_str(),
                row[0].width,
                row[1].width,
                row[2].text.as_str()
            ),
            ("界", 2, 0, "e\u{301}")
        );
        assert!(row[0].style.bold && row[0].style.underline && row[0].style.inverse);
        assert_eq!(row[0].style.foreground, Color::Rgb(205, 0, 0));
        assert_eq!(row[0].style.background, Color::Rgb(1, 34, 80));
        assert!(
            row[3..9]
                .iter()
                .all(|cell| cell.style.hidden && cell.text.is_empty())
        );
        assert_eq!(row[11].text, "r");
        assert_eq!(
            replay.frame(1).unwrap().unwrap().screen.row(0).unwrap()[11].text,
            "n"
        );
        assert_eq!(fs::read_dir(root).unwrap().count(), 2);
    }

    #[test]
    fn java_range_cancel_and_racing_destination_leave_no_staged_file() {
        let scratch = TestDirectory::new();
        let root = scratch.0.canonicalize().unwrap();
        let source = root.join("source.korttyrec");
        capture(&source);
        let output = root.join("excerpt.korttyrec.jsonl.gz");
        let request = request(source, output.clone());
        let canceled = AtomicBool::new(false);
        let error = export(&request, &canceled, |progress| {
            if matches!(progress, Progress::Writing { .. }) {
                canceled.store(true, Ordering::Release);
            }
        })
        .unwrap_err();
        assert!(error.to_string().contains("canceled"));
        assert!(!output.exists());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        canceled.store(false, Ordering::Release);
        assert!(
            export(&request, &canceled, |progress| {
                if matches!(progress, Progress::Publishing) {
                    fs::write(&output, b"existing").unwrap();
                }
            })
            .is_err()
        );
        assert_eq!(fs::read(&output).unwrap(), b"existing");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 2);
    }

    #[test]
    fn java_glyph_mapping_masks_hidden_text_and_preserves_display_width() {
        let mut text = String::new();
        append_glyph(
            &mut text,
            &RecordedCell {
                text: "secret".into(),
                width: 2,
                style: Style {
                    hidden: true,
                    ..Style::default()
                },
            },
        );
        append_glyph(
            &mut text,
            &RecordedCell {
                text: "😀".into(),
                width: 2,
                style: Style::default(),
            },
        );
        append_glyph(
            &mut text,
            &RecordedCell {
                text: "\x1b".into(),
                width: 1,
                style: Style::default(),
            },
        );
        assert_eq!(text, "  😀\u{fffd}");
        let mut limited = Limit::new(Vec::new(), 2);
        assert!(limited.write_all(b"too big").is_err());
        assert!(limited.inner.is_empty());
    }

    #[test]
    #[ignore = "emits a fixture to an explicit KORTTY_JAVA_REPLAY_OUTPUT for the pinned Java reader"]
    fn emit_java_replay_fixture_for_pinned_reader() {
        let output = PathBuf::from(
            std::env::var_os("KORTTY_JAVA_REPLAY_OUTPUT")
                .expect("set an absolute fixture output path"),
        );
        let scratch = TestDirectory::new();
        let source = scratch.0.canonicalize().unwrap().join("source.korttyrec");
        capture(&source);
        let saved = export(
            &request(source, output.clone()),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert_eq!(saved.path, output);
    }
}
