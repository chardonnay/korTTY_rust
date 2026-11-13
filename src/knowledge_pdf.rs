//! Bounded PDF text extraction in a disposable instance of the native desktop program.

use std::fmt;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use lopdf::{Document, LoadOptions};

#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) use tests::{fixture, large_fixture};

pub(crate) const WORKER_ARGUMENT: &str = "--knowledge-pdf-worker";
pub(crate) const MAX_PAGES: usize = 128;
pub(crate) const DEFAULT_FILE_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const PREVIOUS_REVIEWED_FILE_BYTES: usize = 8 * 1024 * 1024;
/// Absolute PDF input limit, including explicitly reviewed sources.
pub(crate) const MAX_FILE_BYTES: usize = 50 * 1024 * 1024;
const MAX_TEXT_BYTES: usize = 128 * 1024;
const MAX_STREAM_BYTES: usize = 1024 * 1024;
const MAX_REPLY_BYTES: usize = 3 + 4 * MAX_PAGES + MAX_TEXT_BYTES;
const TIMEOUT: Duration = Duration::from_secs(8);
const POLL: Duration = Duration::from_millis(10);
static PARSING: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum Failure {
    Malformed = 1,
    Protected = 2,
    NoText = 3,
    Limit = 4,
    Worker = 5,
}

impl Failure {
    fn message(self) -> &'static str {
        match self {
            Self::Malformed => "Invalid PDF or unsupported PDF text encoding",
            Self::Protected => "Encrypted or password-protected PDF",
            Self::NoText => "PDF has no extractable text layer",
            Self::Limit => "PDF exceeds the extraction limits",
            Self::Worker => "The PDF extraction process could not finish",
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(self.message())
    }
}

impl std::error::Error for Failure {}

impl From<Failure> for io::Error {
    fn from(value: Failure) -> Self {
        Self::new(io::ErrorKind::InvalidData, value)
    }
}

pub(crate) fn failure_message(error: &io::Error) -> Option<&'static str> {
    error
        .get_ref()?
        .downcast_ref::<Failure>()
        .map(|reason| reason.message())
}

pub(crate) fn is_pdf(path: &Path) -> bool {
    path.extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
}

pub(crate) fn read_bytes(file: &File) -> io::Result<Vec<u8>> {
    read_bytes_with_limit(file, DEFAULT_FILE_BYTES as u64)
}

/// Bounds bytes read from the already opened source before PDF parsing.
pub(crate) fn read_bytes_with_limit(file: &File, limit: u64) -> io::Result<Vec<u8>> {
    let limit = limit.min(MAX_FILE_BYTES as u64);
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(Failure::Limit.into());
    }
    validate_bytes(&bytes)?;
    Ok(bytes)
}

pub(crate) fn validate_bytes(bytes: &[u8]) -> io::Result<()> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err(Failure::Limit.into());
    }
    if !bytes.starts_with(b"%PDF-") {
        return Err(Failure::Malformed.into());
    }
    Ok(())
}

pub(crate) fn extract(bytes: Vec<u8>, active: impl Fn() -> bool) -> io::Result<Vec<String>> {
    let deadline = Instant::now() + TIMEOUT;
    let _permit = Permit::acquire(deadline, &active)?;
    #[cfg(test)]
    {
        // The test-harness executable has no desktop argument dispatcher. The real
        // process route is separately exercised against the production binary.
        if std::env::var_os("KORTTY_PDF_WORKER_EXE").is_none() {
            let pages = parse(&bytes)?;
            check_active(deadline, &active)?;
            return Ok(pages);
        }
    }
    let exe = std::env::current_exe()?;
    #[cfg(test)]
    let exe = std::env::var_os("KORTTY_PDF_WORKER_EXE").map_or(exe, std::path::PathBuf::from);
    run_worker(&exe, bytes, deadline, &active)
}

struct Permit;

impl Permit {
    fn acquire(deadline: Instant, active: &impl Fn() -> bool) -> io::Result<Self> {
        loop {
            check_active(deadline, active)?;
            if PARSING
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Ok(Self);
            }
            thread::sleep(POLL);
        }
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        PARSING.store(false, Ordering::Release);
    }
}

struct Worker {
    child: Child,
    #[cfg(windows)]
    _job: Option<std::os::windows::io::OwnedHandle>,
}

impl Worker {
    fn start(exe: &Path) -> io::Result<Self> {
        let mut command = Command::new(exe);
        command
            .arg(WORKER_ARGUMENT)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;

            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
            if let Some(root) = std::env::var_os("SystemRoot") {
                command.env("SystemRoot", root);
            }
        }
        let child = command.spawn().map_err(|_| Failure::Worker)?;
        #[cfg(windows)]
        {
            let mut worker = Self { child, _job: None };
            worker._job =
                Some(kortty_pty::contain_pdf_worker(&worker.child).map_err(|_| Failure::Worker)?);
            Ok(worker)
        }
        #[cfg(unix)]
        Ok(Self { child })
    }

    fn stop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_worker(
    exe: &Path,
    bytes: Vec<u8>,
    deadline: Instant,
    active: &impl Fn() -> bool,
) -> io::Result<Vec<String>> {
    check_active(deadline, active)?;
    validate_bytes(&bytes)?;
    let mut worker = Worker::start(exe)?;
    // On Windows the worker must be in its private Job Object before this writer sends bytes.
    let mut input = worker.child.stdin.take().ok_or(Failure::Worker)?;
    let output = worker.child.stdout.take().ok_or(Failure::Worker)?;
    let writer = thread::Builder::new()
        .name("kortty-pdf-input".into())
        .spawn(move || input.write_all(&bytes))?;
    let reader = match thread::Builder::new()
        .name("kortty-pdf-output".into())
        .spawn(move || {
            let mut result = Vec::new();
            output
                .take((MAX_REPLY_BYTES + 1) as u64)
                .read_to_end(&mut result)?;
            decode_reply(&result)
        }) {
        Ok(reader) => reader,
        Err(error) => {
            worker.stop();
            let _ = writer.join();
            return Err(error);
        }
    };
    let exited = loop {
        if let Err(error) = check_active(deadline, active) {
            worker.stop();
            break Err(error);
        }
        match worker.child.try_wait() {
            Ok(Some(status)) if status.success() => break Ok(()),
            Ok(Some(_)) => break Err(Failure::Worker.into()),
            Ok(None) => thread::sleep(POLL),
            Err(_) => {
                worker.stop();
                break Err(Failure::Worker.into());
            }
        }
    };
    let written = writer.join();
    let response = reader.join();
    exited?;
    written
        .map_err(|_| Failure::Worker)?
        .map_err(|_| Failure::Worker)?;
    check_active(deadline, active)?;
    response.map_err(|_| Failure::Worker)?
}

fn check_active(deadline: Instant, active: &impl Fn() -> bool) -> io::Result<()> {
    if !active() {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "PDF extraction was cancelled",
        ));
    }
    if Instant::now() >= deadline {
        return Err(Failure::Limit.into());
    }
    Ok(())
}

pub(crate) fn serve() -> io::Result<()> {
    #[cfg(unix)]
    kortty_pty::limit_pdf_worker()?;
    let mut bytes = Vec::new();
    io::stdin()
        .lock()
        .take((MAX_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    encode_reply(&mut io::stdout().lock(), parse(&bytes))
}

fn parse(bytes: &[u8]) -> Result<Vec<String>, Failure> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err(Failure::Limit);
    }
    if !bytes.starts_with(b"%PDF-") {
        return Err(Failure::Malformed);
    }
    let mut options = LoadOptions::with_max_decompressed_size(MAX_STREAM_BYTES);
    options.strict = true;
    let document = Document::load_mem_with_options(bytes, options).map_err(parser_failure)?;
    if document.was_encrypted() || document.is_encrypted() {
        return Err(Failure::Protected);
    }
    let declared = document
        .catalog()
        .and_then(|catalog| catalog.get_deref(b"Pages", &document))
        .and_then(lopdf::Object::as_dict)
        .and_then(|pages| pages.get(b"Count"))
        .and_then(lopdf::Object::as_i64)
        .map_err(parser_failure)?;
    let declared = usize::try_from(declared).map_err(|_| Failure::Malformed)?;
    if declared > MAX_PAGES {
        return Err(Failure::Limit);
    }
    let pages = document.get_pages();
    if pages.len() != declared
        || pages
            .values()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != declared
    {
        return Err(Failure::Malformed);
    }
    let mut text_bytes = 0;
    let mut text_pages = Vec::with_capacity(pages.len());
    for (position, page) in pages.into_keys().enumerate() {
        if page as usize != position + 1 {
            return Err(Failure::Malformed);
        }
        let text = document
            .extract_text_with_limit(&[page], MAX_STREAM_BYTES)
            .map_err(parser_failure)?;
        let mut text = text.replace("\r\n", "\n").replace('\r', "\n");
        text.retain(|ch| !ch.is_control() || matches!(ch, '\n' | '\t'));
        text_bytes += text.len();
        if text_bytes > MAX_TEXT_BYTES {
            return Err(Failure::Limit);
        }
        text_pages.push(text);
    }
    if text_pages.iter().all(|page| page.trim().is_empty()) {
        return Err(Failure::NoText);
    }
    Ok(text_pages)
}

fn parser_failure(error: lopdf::Error) -> Failure {
    match error {
        lopdf::Error::InvalidPassword
        | lopdf::Error::Decryption(_)
        | lopdf::Error::UnsupportedSecurityHandler(_) => Failure::Protected,
        lopdf::Error::Decompress(lopdf::DecompressError::MemoryLimitExceeded { .. }) => {
            Failure::Limit
        }
        _ => Failure::Malformed,
    }
}

fn encode_reply(output: &mut impl Write, result: Result<Vec<String>, Failure>) -> io::Result<()> {
    let pages = match result {
        Ok(pages) => pages,
        Err(error) => {
            output.write_all(&[error as u8])?;
            return output.flush();
        }
    };
    output.write_all(&[0])?;
    output.write_all(&(pages.len() as u16).to_le_bytes())?;
    for page in pages {
        output.write_all(&(page.len() as u32).to_le_bytes())?;
        output.write_all(page.as_bytes())?;
    }
    output.flush()
}

fn decode_reply(bytes: &[u8]) -> io::Result<Vec<String>> {
    let Some((&status, mut body)) = bytes
        .split_first()
        .filter(|_| bytes.len() <= MAX_REPLY_BYTES)
    else {
        return Err(Failure::Worker.into());
    };
    match status {
        0 => {}
        1 if body.is_empty() => return Err(Failure::Malformed.into()),
        2 if body.is_empty() => return Err(Failure::Protected.into()),
        3 if body.is_empty() => return Err(Failure::NoText.into()),
        4 if body.is_empty() => return Err(Failure::Limit.into()),
        _ => return Err(Failure::Worker.into()),
    }
    let count = take::<2>(&mut body)
        .map(u16::from_le_bytes)
        .ok_or(Failure::Worker)? as usize;
    if !(1..=MAX_PAGES).contains(&count) {
        return Err(Failure::Worker.into());
    }
    let mut pages = Vec::with_capacity(count);
    let mut remaining = MAX_TEXT_BYTES;
    for _ in 0..count {
        let len = take::<4>(&mut body)
            .map(u32::from_le_bytes)
            .ok_or(Failure::Worker)? as usize;
        remaining = remaining.checked_sub(len).ok_or(Failure::Worker)?;
        let (page, rest) = body.split_at_checked(len).ok_or(Failure::Worker)?;
        let page = std::str::from_utf8(page).map_err(|_| Failure::Worker)?;
        if page
            .chars()
            .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\t'))
        {
            return Err(Failure::Worker.into());
        }
        pages.push(page.to_owned());
        body = rest;
    }
    if !body.is_empty() || pages.iter().all(|page| page.trim().is_empty()) {
        return Err(Failure::Worker.into());
    }
    Ok(pages)
}

fn take<const N: usize>(bytes: &mut &[u8]) -> Option<[u8; N]> {
    let (head, tail) = bytes.split_first_chunk::<N>()?;
    *bytes = tail;
    Some(*head)
}
