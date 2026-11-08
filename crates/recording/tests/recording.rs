use std::fs;
use std::io::{self, Cursor, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use kortty_recording::{CaptureOptions, Error, Recorder, Replay, State};
use kortty_terminal::{Color, Style, Terminal};

fn seconds(value: u64) -> Duration {
    Duration::from_secs(value)
}

fn nanos(value: u64) -> Duration {
    Duration::from_nanos(value)
}

#[test]
fn records_visible_wide_and_combining_cells_without_concealed_or_link_data() {
    let mut terminal = Terminal::new(24, 2);
    terminal.advance(b"\x1b[31;1m\x1b]8;;https://private.example/hidden\x1b\\");
    terminal.advance("界e\u{301}".as_bytes());
    terminal.advance(b"\x1b]8;;\x1b\\\x1b[8mSECRET\x1b[28m!");
    terminal.advance(b"\x1b[5 q\x1b[?5h");
    let screen = terminal.snapshot();
    assert_eq!(screen.row(0).unwrap()[3].text, "S");
    assert!(screen.row(0).unwrap()[3].style.hidden);

    for include_style in [false, true] {
        let options = CaptureOptions {
            include_style,
            idle_after: None,
        };
        let mut writer = Recorder::new(Vec::new(), options).unwrap();
        writer.start(Duration::ZERO).unwrap();
        assert!(
            writer
                .record_screen(nanos(1), 7, &screen, (390, 48))
                .unwrap()
        );
        assert!(
            !writer
                .record_screen(nanos(2), 7, &screen, (390, 48))
                .unwrap()
        );
        writer.input_activity(nanos(3)).unwrap();
        let bytes = writer.finish(nanos(4)).unwrap();
        assert!(!bytes.windows(15).any(|chunk| chunk == b"private.example"));
        let mut replay = Replay::from_reader(Cursor::new(bytes)).unwrap();
        assert!(replay.is_complete());
        assert_eq!(replay.frame_count(), 1);
        assert_eq!(replay.duration(), nanos(3));
        let frame = replay.seek(Duration::ZERO).unwrap().unwrap();
        assert_eq!(frame.pane, 7);
        assert_eq!(frame.screen.pixels, (390, 48));
        assert_eq!(frame.screen.cursor, screen.cursor);
        assert_eq!(frame.screen.cursor_shape, screen.cursor_shape);
        assert_eq!(frame.screen.cursor_blinking, screen.cursor_blinking);
        assert_eq!(frame.screen.reverse_video, include_style);
        let row = frame.screen.row(0).unwrap();
        assert_eq!(row[0].text, "界");
        assert_eq!(row[0].width, 2);
        assert_eq!(row[1].width, 0);
        assert_eq!(row[2].text, "e\u{301}");
        assert!(row[3..9].iter().all(|cell| cell.text.is_empty()));
        assert_eq!(row[9].text, "!");
        if include_style {
            assert_eq!(row[0].style.foreground, Color::Indexed(1));
            assert!(row[0].style.bold);
        } else {
            assert!(row.iter().all(|cell| cell.style == Style::default()));
        }
    }
}

#[test]
fn seeking_omits_paused_and_stopped_time_without_rounding_raw_timestamps() {
    let options = CaptureOptions {
        include_style: false,
        idle_after: Some(seconds(2)),
    };
    let mut writer = Recorder::new(Vec::new(), options).unwrap();
    let epoch = nanos(9_007_199_254_740_993);
    let mut terminal = Terminal::new(6, 1);
    writer.start(epoch).unwrap();
    terminal.advance(b"one");
    writer
        .record_screen(epoch + nanos(1), 1, &terminal.snapshot(), (0, 0))
        .unwrap();
    assert!(
        !writer
            .record_screen(epoch + seconds(1), 1, &terminal.snapshot(), (0, 0))
            .unwrap()
    );
    writer.check_idle(epoch + seconds(2) + nanos(1)).unwrap();
    assert_eq!(writer.state(), State::AutoPaused);
    assert!(
        !writer
            .record_screen(epoch + seconds(12), 1, &terminal.snapshot(), (0, 0))
            .unwrap()
    );
    assert_eq!(writer.state(), State::AutoPaused);
    writer.input_activity(epoch + seconds(20)).unwrap();
    assert_eq!(writer.state(), State::Recording);

    terminal.advance(b"\rtwo");
    let second_time = epoch + seconds(20) + nanos(7);
    writer
        .record_screen(second_time, 2, &terminal.snapshot(), (0, 0))
        .unwrap();
    writer.stop(epoch + seconds(21)).unwrap();
    assert_eq!(writer.state(), State::Stopped);
    assert!(
        !writer
            .record_screen(epoch + seconds(30), 3, &terminal.snapshot(), (0, 0))
            .unwrap()
    );
    writer.start(epoch + seconds(40)).unwrap();
    assert!(
        writer
            .record_screen(
                epoch + seconds(40) + nanos(5),
                2,
                &terminal.snapshot(),
                (0, 0)
            )
            .unwrap()
    );
    let data = writer.finish(epoch + seconds(41)).unwrap();

    let mut replay = Replay::from_reader(Cursor::new(data)).unwrap();
    assert_eq!(replay.frame_count(), 3);
    assert_eq!(replay.duration(), seconds(4));
    assert_eq!(replay.seek(seconds(2)).unwrap().unwrap().pane, 1);
    let second = replay.seek(seconds(2) + nanos(7)).unwrap().unwrap();
    assert_eq!(second.recorded_at, second_time);
    assert_eq!(second.playback_at, seconds(2) + nanos(7));
    assert_eq!(second.pane, 2);
    assert_eq!(
        replay.seek(Duration::MAX).unwrap().unwrap().playback_at,
        seconds(3) + nanos(5)
    );
}

#[test]
fn changed_screen_resumes_idle_and_equal_timestamps_use_the_last_frame() {
    let mut writer = Recorder::new(
        Vec::new(),
        CaptureOptions {
            idle_after: Some(seconds(1)),
            ..CaptureOptions::default()
        },
    )
    .unwrap();
    let mut terminal = Terminal::new(3, 1);
    writer.start(Duration::ZERO).unwrap();
    writer
        .record_screen(Duration::ZERO, 1, &terminal.snapshot(), (0, 0))
        .unwrap();
    writer.check_idle(seconds(1)).unwrap();
    terminal.advance(b"A");
    writer
        .record_screen(seconds(30), 1, &terminal.snapshot(), (0, 0))
        .unwrap();
    assert_eq!(writer.state(), State::Recording);
    terminal.advance(b"B");
    writer
        .record_screen(seconds(30), 1, &terminal.snapshot(), (0, 0))
        .unwrap();
    let data = writer.finish(seconds(31)).unwrap();
    let mut replay = Replay::from_reader(Cursor::new(data)).unwrap();
    assert_eq!(replay.duration(), seconds(2));
    assert_eq!(replay.frame(1).unwrap().unwrap().playback_at, seconds(1));
    assert_eq!(
        replay.seek(seconds(1)).unwrap().unwrap().screen.cells[1].text,
        "B"
    );
}

#[test]
fn opens_private_files_and_exports_a_range_without_overwriting() {
    let directory = TestDirectory::new();
    let source = directory.0.join("source.korttyrec");
    let target = directory.0.join("clip.korttyrec");
    let mut writer = Recorder::create(&source, CaptureOptions::default()).unwrap();
    let mut terminal = Terminal::new(3, 1);
    writer.start(Duration::ZERO).unwrap();
    for (at, letter) in [(0, b'A'), (2, b'B'), (5, b'C')] {
        terminal.advance(&[b'\r', letter]);
        writer
            .record_screen(seconds(at), 1, &terminal.snapshot(), (0, 0))
            .unwrap();
    }
    writer.flush().unwrap();
    let partial = Replay::open(&source).unwrap();
    assert_eq!(partial.frame_count(), 3);
    assert!(!partial.is_complete());
    drop(partial);
    writer.finish(seconds(8)).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(source.metadata().unwrap().permissions().mode() & 0o077, 0);
    }
    let mut replay = Replay::open(&source).unwrap();
    replay
        .export_range(&target, seconds(1)..seconds(4))
        .unwrap();
    let mut clip = Replay::open(&target).unwrap();
    assert_eq!(clip.duration(), seconds(3));
    assert_eq!(clip.frame_count(), 2);
    assert_eq!(
        clip.seek(Duration::ZERO).unwrap().unwrap().screen.cells[0].text,
        "A"
    );
    assert_eq!(
        clip.seek(seconds(1)).unwrap().unwrap().screen.cells[0].text,
        "B"
    );
    let original = fs::read(&target).unwrap();
    assert!(
        matches!(replay.export_range(&target, Duration::ZERO..seconds(8)), Err(Error::Io(error)) if error.kind() == io::ErrorKind::AlreadyExists)
    );
    assert_eq!(fs::read(&target).unwrap(), original);
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 2);
}

#[test]
fn detects_bad_records_and_keeps_frames_before_a_truncated_tail() {
    let mut writer = Recorder::new(Vec::new(), CaptureOptions::default()).unwrap();
    writer.start(seconds(2)).unwrap();
    assert!(matches!(
        writer.input_activity(seconds(1)),
        Err(Error::Invalid(_))
    ));
    writer
        .record_screen(seconds(2), 1, &Terminal::new(1, 1).snapshot(), (0, 0))
        .unwrap();
    let bytes = writer.finish(seconds(3)).unwrap();
    let replay = Replay::from_reader(Cursor::new(&bytes[..bytes.len() - 5])).unwrap();
    assert!(!replay.is_complete());
    assert_eq!(replay.frame_count(), 1);

    let mut invalid = bytes.clone();
    invalid[9] = 10;
    assert!(matches!(
        Replay::from_reader(Cursor::new(invalid)),
        Err(Error::Format(_))
    ));

    let mut invalid = bytes.clone();
    invalid[34..38].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(matches!(
        Replay::from_reader(Cursor::new(invalid)),
        Err(Error::Format(_))
    ));

    let mut invalid = bytes;
    invalid[59..63].fill(0);
    let mut replay = Replay::from_reader(Cursor::new(invalid)).unwrap();
    assert!(matches!(replay.frame(0), Err(Error::Format(_))));
    let directory = TestDirectory::new();
    let output = directory.0.join("invalid.korttyrec");
    assert!(matches!(
        replay.export_range(&output, Duration::ZERO..seconds(1)),
        Err(Error::Format(_))
    ));
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 0);
}

#[test]
fn a_partial_io_failure_prevents_followup_events() {
    // A test double that succeeds for the header and then stops accepting writes.
    struct FailsAfterHeader(usize);
    impl Write for FailsAfterHeader {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.0 == 0 {
                return Err(io::Error::other("test disk failure"));
            }
            let wrote = bytes.len().min(self.0);
            self.0 -= wrote;
            Ok(wrote)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Recorder::new(FailsAfterHeader(13), CaptureOptions::default()).unwrap();
    assert!(matches!(writer.start(Duration::ZERO), Err(Error::Io(_))));
    assert_eq!(writer.state(), State::Idle);
    assert!(matches!(writer.finish(seconds(1)), Err(Error::Poisoned)));
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nonce = NEXT.fetch_add(1, Ordering::Relaxed);
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "kortty-recording-test-{}-{time}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
