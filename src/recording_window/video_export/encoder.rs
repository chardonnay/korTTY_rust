//! Fixed FFmpeg invocation, bounded progress and process cleanup.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Cursor, Read, Seek};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use super::{Format, MAX_VIDEO_BYTES, Progress, active, approval::Proof, catalog, invalid};
use crate::private_files::{FileStamp, require_single_link};

const TIMEOUT: Duration = Duration::from_secs(5 * 60);
const POLL: Duration = Duration::from_millis(40);
const MAX_PROGRESS: u64 = 1024 * 1024;
const MAX_HEADERS: u64 = 128 * 1024;

pub(super) fn run(
    executable: &Proof,
    scratch: &Path,
    format: Format,
    total_ms: u64,
    canceled: &AtomicBool,
    mut report: impl FnMut(Progress),
) -> io::Result<(File, FileStamp)> {
    let progress_path = scratch.join("progress.log");
    let log = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&progress_path)?;
    let mut log_reader = catalog::open_regular(&progress_path)?;
    let output_path = scratch.join(output_name(format));
    let mut command = command(executable.path(), scratch, format, total_ms);
    command.stdout(Stdio::from(log));
    executable.recheck()?;
    active(canceled)?;
    let mut child = Process::spawn(&mut command)?;
    let start = Instant::now();
    let mut last_ms = 0;
    report(Progress::Encoding {
        elapsed_ms: 0,
        total_ms,
    });
    loop {
        active(canceled)?;
        if start.elapsed() >= TIMEOUT {
            return Err(invalid("FFmpeg exceeded the five-minute encoding timeout"));
        }
        require_output_bound(&output_path)?;
        let status = child.try_wait()?;
        let progress = read_progress(&mut log_reader)?;
        if let Some(milliseconds) = progress.milliseconds.filter(|&time| time > last_ms) {
            last_ms = milliseconds;
            report(Progress::Encoding {
                elapsed_ms: milliseconds.min(total_ms),
                total_ms,
            });
        }
        if let Some(status) = status {
            if !status.success() {
                return Err(invalid(
                    "FFmpeg failed. Check the configured build supports the selected codec",
                ));
            }
            if !progress.finished || last_ms.saturating_add(2) < total_ms || last_ms > total_ms + 2
            {
                return Err(invalid(
                    "FFmpeg did not complete the full selected time range",
                ));
            }
            break;
        }
        std::thread::sleep(POLL);
    }
    drop(child);
    active(canceled)?;
    let mut output = catalog::open_regular(&output_path)?;
    require_single_link(&output)?;
    let stamp = FileStamp::read(&output)?;
    if !(1..=MAX_VIDEO_BYTES).contains(&output.metadata()?.len()) {
        return Err(invalid("encoded video is empty or exceeds its size limit"));
    }
    verify_container(&mut output, format)?;
    stamp.require_unchanged(&output)?;
    output.rewind()?;
    Ok((output, stamp))
}

fn output_name(format: Format) -> &'static str {
    match format {
        Format::Webm => "encoded.webm",
        Format::Mkv => "encoded.mkv",
        Format::Native | Format::Java => unreachable!("replay export does not invoke FFmpeg"),
    }
}

fn command(executable: &Path, scratch: &Path, format: Format, total_ms: u64) -> Command {
    let mut command = Command::new(executable);
    command
        .current_dir(scratch)
        .env_clear()
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    for key in ["TEMP", "TMP", "TMPDIR"] {
        command.env(key, scratch);
    }
    #[cfg(windows)]
    for key in ["SystemRoot", "WINDIR"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command.args([
        "-hide_banner",
        "-nostdin",
        "-loglevel",
        "error",
        "-nostats",
        "-n",
        "-stats_period",
        "0.25",
        "-progress",
        "pipe:1",
        "-filter_threads",
        "2",
        // The ffconcat file contains only generated relative filenames and a fixed
        // framerate option, which the concat demuxer rejects in safe mode.
        "-f",
        "concat",
        "-safe",
        "0",
        "-protocol_whitelist",
        "file",
        "-i",
        "frames.ffconcat",
        "-map",
        "0:v:0",
        "-map_metadata",
        "-1",
        "-map_chapters",
        "-1",
        "-an",
        "-sn",
        "-dn",
        "-fps_mode:v",
        "vfr",
        "-enc_time_base:v",
        "1:1000",
        "-threads:v",
        "4",
    ]);
    match format {
        Format::Webm => command.args([
            "-c:v",
            "libvpx-vp9",
            "-row-mt",
            "1",
            "-cpu-used",
            "4",
            "-pix_fmt",
            "yuv420p",
            "-f",
            "webm",
        ]),
        Format::Mkv => command.args([
            "-c:v", "ffv1", "-level", "3", "-g", "1", "-pix_fmt", "bgr0", "-f", "matroska",
        ]),
        Format::Native | Format::Java => unreachable!("replay export does not invoke FFmpeg"),
    };
    command
        .arg("-t")
        .arg(format!("{}.{:03}", total_ms / 1000, total_ms % 1000))
        .arg("-fs")
        .arg(MAX_VIDEO_BYTES.to_string())
        .arg(output_name(format));
    command
}

fn require_output_bound(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() || metadata.len() > MAX_VIDEO_BYTES => {
            Err(invalid("FFmpeg output is not a bounded regular file"))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[derive(Default)]
struct EncoderProgress {
    milliseconds: Option<u64>,
    finished: bool,
}

fn read_progress(file: &mut File) -> io::Result<EncoderProgress> {
    file.rewind()?;
    let mut bytes = Vec::new();
    file.take(MAX_PROGRESS + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_PROGRESS {
        return Err(invalid("FFmpeg progress exceeded its output limit"));
    }
    let mut progress = EncoderProgress::default();
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        let Some(line) = line.strip_suffix(b"\n") else {
            break;
        };
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if let Some(time) = line.strip_prefix(b"out_time_us=")
            && let Ok(time) = std::str::from_utf8(time)
            && let Ok(time) = time.parse::<u64>()
        {
            progress.milliseconds = Some(time / 1000);
        }
        if line == b"progress=end" {
            progress.finished = true;
        }
    }
    Ok(progress)
}

fn verify_container(input: &mut File, format: Format) -> io::Result<()> {
    let mut input = input.take(MAX_HEADERS);
    let (header, size) = element(&mut input)?;
    if header != 0x1a45dfa3 || size > 4096 {
        return Err(invalid("FFmpeg output is missing its EBML header"));
    }
    let header = read_body(&mut input, size)?;
    let (doctype, codec): (&[u8], &[u8]) = match format {
        Format::Webm => (b"webm", b"V_VP9"),
        Format::Mkv => (b"matroska", b"V_FFV1"),
        Format::Native | Format::Java => unreachable!(),
    };
    if attribute(&header, 0x4282)? != Some(doctype) || element(&mut input)?.0 != 0x18538067 {
        return Err(invalid("FFmpeg output has the wrong video container"));
    }
    loop {
        let (id, size) = element(&mut input)?;
        if id == 0x1654ae6b {
            let tracks = read_body(&mut input, size)?;
            let entry = attribute(&tracks, 0xae)?
                .ok_or_else(|| invalid("FFmpeg output has no video track"))?;
            let actual = attribute(entry, 0x86)?;
            let fourcc = format == Format::Mkv
                && actual == Some(b"V_MS/VFW/FOURCC".as_slice())
                && attribute(entry, 0x63a2)?
                    .is_some_and(|private| private.len() >= 40 && &private[16..20] == b"FFV1");
            if attribute(entry, 0x83)? != Some(b"\x01".as_slice())
                || (actual != Some(codec) && !fourcc)
            {
                return Err(invalid(
                    "FFmpeg output does not contain the requested video codec",
                ));
            }
            return Ok(());
        }
        if id == 0x1f43b675
            || size > input.limit()
            || io::copy(&mut (&mut input).take(size), &mut io::sink())? != size
        {
            return Err(invalid(
                "FFmpeg output is missing a bounded video track header",
            ));
        }
    }
}

fn read_body(input: &mut impl Read, size: u64) -> io::Result<Vec<u8>> {
    if size > MAX_HEADERS {
        return Err(invalid("FFmpeg output has an oversized video header"));
    }
    let mut bytes = vec![0; size as usize];
    input.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn attribute(bytes: &[u8], wanted: u64) -> io::Result<Option<&[u8]>> {
    let mut input = Cursor::new(bytes);
    let mut found = None;
    while input.position() < bytes.len() as u64 {
        let (id, size) = element(&mut input)?;
        let start = input.position();
        let end = start
            .checked_add(size)
            .filter(|end| *end <= bytes.len() as u64)
            .ok_or_else(|| invalid("invalid EBML field in FFmpeg output"))?;
        if id == wanted {
            if found.is_some() {
                return Err(invalid("duplicate video header field in FFmpeg output"));
            }
            found = Some(&bytes[start as usize..end as usize]);
        }
        input.set_position(end);
    }
    Ok(found)
}

fn element(input: &mut impl Read) -> io::Result<(u64, u64)> {
    Ok((
        variable_integer(input, 4, true)?,
        variable_integer(input, 8, false)?,
    ))
}

fn variable_integer(input: &mut impl Read, maximum: u32, identifier: bool) -> io::Result<u64> {
    let mut byte = [0];
    input.read_exact(&mut byte)?;
    let width = byte[0].leading_zeros() + 1;
    if width > maximum {
        return Err(invalid("invalid EBML integer in FFmpeg output"));
    }
    let mut value = u64::from(if identifier {
        byte[0]
    } else {
        byte[0] & !(1u8 << (8 - width))
    });
    for _ in 1..width {
        input.read_exact(&mut byte)?;
        value = value * 256 + u64::from(byte[0]);
    }
    Ok(value)
}

struct Process {
    child: Child,
    #[cfg(unix)]
    group: Option<rustix::process::Pid>,
    #[cfg(windows)]
    job: Option<std::os::windows::io::OwnedHandle>,
}

impl Process {
    fn spawn(command: &mut Command) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            command.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let child = command.spawn()?;
        #[cfg(unix)]
        {
            let group = rustix::process::Pid::from_raw(child.id() as i32);
            Ok(Self { child, group })
        }
        #[cfg(windows)]
        {
            let mut process = Self { child, job: None };
            match kortty_pty::contain_child(&process.child) {
                Ok(job) => process.job = Some(job),
                Err(_) if matches!(process.child.try_wait(), Ok(Some(_))) => {}
                Err(error) => return Err(error),
            }
            Ok(process)
        }
    }

    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let status = self.child.try_wait()?;
        #[cfg(unix)]
        if status.is_some() {
            self.group = None;
        }
        Ok(status)
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        #[cfg(windows)]
        drop(self.job.take());
        #[cfg(unix)]
        if let Some(group) = self.group.take() {
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        }
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropping_the_encoder_process_reaps_a_running_child() {
        let scratch = crate::recording_window::tests::TestDirectory::new();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .current_dir(&scratch.0)
            .env("KORTTY_TEST_VIDEO_ENCODER_CHILD", "1")
            .args([
                "--ignored",
                "--exact",
                "recording_window::video_export::encoder::tests::encoder_fixture_child",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = Process::spawn(&mut command).unwrap();
        let start = Instant::now();
        while !scratch.0.join("running").exists() {
            assert!(
                child.try_wait().unwrap().is_none(),
                "test encoder exited before starting"
            );
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "test encoder did not start"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let canceled_at = Instant::now();
        drop(child);
        assert!(canceled_at.elapsed() < Duration::from_secs(10));
    }

    #[test]
    #[ignore = "subprocess fixture"]
    fn encoder_fixture_child() {
        if std::env::var_os("KORTTY_TEST_VIDEO_ENCODER_CHILD").is_some() {
            fs::write("running", b"started").unwrap();
            std::thread::sleep(Duration::from_secs(30));
        }
    }
}
