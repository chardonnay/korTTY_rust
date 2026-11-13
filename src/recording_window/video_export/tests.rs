use super::*;

use std::io::BufReader;
use std::process::Command;

use kortty_recording::{CaptureOptions, Recorder};
use kortty_terminal::Terminal;

use crate::recording_window::tests::TestDirectory;

fn at(millis: u64) -> Duration {
    Duration::from_millis(millis)
}

fn capture(path: &Path) -> Replay {
    let mut writer = Recorder::create(
        path,
        CaptureOptions {
            include_style: true,
            idle_after: None,
        },
    )
    .unwrap();
    let mut left = Terminal::new(16, 2);
    let mut right = Terminal::new(50, 2);
    left.advance(b"\x1b[1;48;2;190;20;30mleft\x1b[0m");
    right.advance(b"right");
    writer.start(at(0)).unwrap();
    writer
        .record_screen(at(0), 9, &right.snapshot(), (460, 55))
        .unwrap();
    writer
        .record_screen(at(0), 3, &left.snapshot(), (180, 63))
        .unwrap();
    right.advance(b" two");
    writer
        .record_screen(at(50), 9, &right.snapshot(), (460, 55))
        .unwrap();
    writer.pause(at(100)).unwrap();
    writer.resume(at(1100)).unwrap();
    left.advance(b" two");
    writer
        .record_screen(at(1150), 3, &left.snapshot(), (180, 63))
        .unwrap();
    right.advance(b" three");
    writer
        .record_screen(at(1150), 9, &right.snapshot(), (460, 55))
        .unwrap();
    left.advance(b"!");
    writer
        .record_screen(
            at(1150) + Duration::from_micros(500),
            3,
            &left.snapshot(),
            (190, 63),
        )
        .unwrap();
    left.advance(b" end");
    writer
        .record_screen(at(1300), 3, &left.snapshot(), (190, 63))
        .unwrap();
    writer.finish(at(1500)).unwrap();
    Replay::open(path).unwrap()
}

#[test]
fn vfr_uses_native_playback_time_and_a_stable_even_multi_pane_canvas() {
    let scratch = TestDirectory::new();
    let mut replay = capture(&scratch.0.join("timeline.korttyrec"));
    let canceled = AtomicBool::new(false);
    let timeline = timing::Timeline::new(&replay, at(25)..at(300), &canceled).unwrap();
    assert_eq!(timeline.millis, 275);
    assert_eq!(
        timeline
            .samples
            .iter()
            .map(|sample| sample.millis)
            .collect::<Vec<_>>(),
        [0, 25, 125]
    );
    assert_eq!(timeline.samples[2].at, at(150) + Duration::from_micros(500));
    assert!(timeline.hold_last());
    let layout = timing::Layout::new(&mut replay, &timeline, &canceled).unwrap();
    assert_eq!(layout.dimensions, (656, 64));
    assert_eq!(layout.origins, [(0, 0), (196, 0)]);

    let root = scratch.0.canonicalize().unwrap();
    let staging = Scratch::new(&root).unwrap();
    render_frames(
        &mut replay,
        &staging,
        &layout,
        &timeline,
        true,
        &canceled,
        |_| {},
    )
    .unwrap();
    let manifest = fs::read_to_string(staging.path.join("frames.ffconcat")).unwrap();
    assert_eq!(
        manifest,
        concat!(
            "ffconcat version 1.0\nfile frame-000000.png\noption framerate 1000\nduration 0.025\n",
            "file frame-000001.png\noption framerate 1000\nduration 0.100\n",
            "file frame-000002.png\noption framerate 1000\nduration 0.149\n",
            "file frame-000002.png\noption framerate 1000\n",
        )
    );
    let first =
        tiny_skia::Pixmap::decode_png(&fs::read(staging.path.join("frame-000000.png")).unwrap())
            .unwrap();
    assert_eq!((first.width(), first.height()), layout.dimensions);
    assert_eq!(first.pixel(5, 5).unwrap().red(), 190);
    assert_ne!(first.pixel(201, 5).unwrap().red(), 190);
    let path = staging.path.clone();
    drop(staging);
    assert!(!path.exists());
}

fn request(root: &Path, executable: PathBuf, format: Format) -> Request {
    let source_path = root.join("source.korttyrec");
    let replay = if source_path.exists() {
        Replay::open(&source_path).unwrap()
    } else {
        capture(&source_path)
    };
    let file = catalog::open_regular(&source_path).unwrap();
    Request {
        source: Arc::new(Mutex::new(replay)),
        source_path,
        source_stamp: FileStamp::read(&file).unwrap(),
        output: root.join(format!("output.{}", format.extension())),
        range: at(25)..at(300),
        format,
        include_color: false,
        executable: Some(Selection {
            sha256: fingerprint(&executable).unwrap(),
            path: executable,
        }),
    }
}

#[test]
fn rendering_cancellation_and_existing_output_leave_no_video_or_staging() {
    let scratch = TestDirectory::new();
    let root = scratch.0.canonicalize().unwrap();
    let executable = fixture_encoder(&root);
    let request = request(&root, executable, Format::Webm);
    let canceled = AtomicBool::new(false);
    let result = export(
        &request,
        &root,
        &canceled,
        |progress| {
            if matches!(progress, Progress::Rendering { .. }) {
                canceled.store(true, Ordering::Release);
            }
        },
        || Ok(()),
    );
    assert!(result.unwrap_err().to_string().contains("canceled"));
    assert!(!request.output.exists());
    assert_eq!(fs::read_dir(&root).unwrap().count(), 2);

    fs::write(&request.output, b"existing").unwrap();
    canceled.store(false, Ordering::Release);
    let result = export(
        &request,
        &root,
        &canceled,
        |_| panic!("must reject an existing file before rendering"),
        || Ok(()),
    );
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read(&request.output).unwrap(), b"existing");
    assert_eq!(fs::read_dir(&root).unwrap().count(), 3);
}

#[test]
fn a_configured_video_route_uses_the_existing_range_controls_without_a_default_encoder() {
    use iced::futures::{StreamExt, executor::block_on};
    use iced_runtime::{Action, task};

    let scratch = TestDirectory::new();
    let root = scratch.0.canonicalize().unwrap();
    let path = root.join("replay.korttyrec");
    capture(&path);
    let mut viewer = super::super::RecordingWindow::replay(path, true);
    let mut outcomes = task::into_stream(viewer.initialize()).unwrap();
    let Some(Action::Output(opened @ Message::Opened(Ok(_)))) = block_on(outcomes.next()) else {
        panic!("expected a decoded recording");
    };
    let _ = viewer.update(opened);
    let _ = viewer.update(Message::ExportFormat(Format::Webm));
    assert_eq!(viewer.video.format, Format::Native);
    let executable = fixture_encoder(&root);
    viewer.set_video_encoder(Some(Selection {
        sha256: fingerprint(&executable).unwrap(),
        path: executable,
    }));
    let _ = viewer.update(Message::ExportFormat(Format::Mkv));
    let _ = viewer.update(Message::ExportColors(false));
    let _ = viewer.update(Message::ExportStart("0:02".into()));
    let _ = viewer.update(Message::ExportEnd("0:03".into()));
    let output = root.join("out.mkv");
    let _ = viewer.update(Message::ExportPath(output.to_str().unwrap().into()));
    assert_eq!(viewer.video.format, Format::Mkv);
    assert!(viewer.video.colorless);
    assert!(task::into_stream(viewer.update(Message::Export)).is_none());
    assert!(
        viewer
            .notice
            .as_deref()
            .unwrap()
            .contains("valid export range")
    );
    assert!(!output.exists());
}

#[test]
#[ignore = "requires explicit absolute KORTTY_TEST_FFMPEG and KORTTY_TEST_FFPROBE executables"]
fn opt_in_ffmpeg_encodes_both_video_formats_with_exact_native_range() {
    let executable = |key| {
        resolve_encoder(Path::new(
            &std::env::var_os(key).expect("configure the test executable"),
        ))
        .unwrap()
    };
    let ffmpeg = executable("KORTTY_TEST_FFMPEG");
    let ffprobe = executable("KORTTY_TEST_FFPROBE");
    let scratch = TestDirectory::new();
    let root = scratch.0.canonicalize().unwrap();
    for (format, codec) in [(Format::Webm, "vp9"), (Format::Mkv, "ffv1")] {
        let request = request(&root, ffmpeg.clone(), format);
        let saved = export(&request, &root, &AtomicBool::new(false), |_| {}, || Ok(())).unwrap();
        assert_eq!(saved.path, request.output);
        let probe = Command::new(&ffprobe)
            .args([
                "-v",
                "error",
                "-show_packets",
                "-show_entries",
                "format=duration:stream=codec_name,width,height:packet=pts_time",
                "-of",
                "json",
            ])
            .arg(&saved.path)
            .output()
            .unwrap();
        assert!(probe.status.success());
        let details: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();
        assert_eq!(details["streams"][0]["codec_name"], codec);
        assert_eq!(details["streams"][0]["width"], 656);
        assert_eq!(details["streams"][0]["height"], 64);
        assert_eq!(details["format"]["duration"], "0.275000");
        let mut ticks = details["packets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|packet| {
                (packet["pts_time"].as_str().unwrap().parse::<f64>().unwrap() * 1000.0).round()
                    as u64
            })
            .collect::<Vec<_>>();
        ticks.sort_unstable();
        assert_eq!(ticks, [0, 25, 125, 274]);
        assert!(
            Command::new(&ffmpeg)
                .args(["-v", "error", "-xerror", "-nostdin", "-i"])
                .arg(&saved.path)
                .args(["-f", "null", "-"])
                .status()
                .unwrap()
                .success()
        );
        let mut native = Replay::from_reader(BufReader::new(
            catalog::open_regular(&request.source_path).unwrap(),
        ))
        .unwrap();
        assert!(native.frame(0).unwrap().is_some());
    }
    assert_eq!(fs::read_dir(&root).unwrap().count(), 3);
}
