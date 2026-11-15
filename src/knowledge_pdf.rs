//! Bounded PDF text extraction in a disposable instance of the native desktop program.

use std::fmt;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

use lopdf::{Document, LoadOptions, Object, ObjectId};
use sha2::{Digest, Sha256};

#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) use tests::{fixture, large_fixture};

pub(crate) const WORKER_ARGUMENT: &str = "--knowledge-pdf-worker";
pub(crate) const MAX_PAGES: usize = 128;
pub(crate) const DEFAULT_FILE_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const PREVIOUS_REVIEWED_FILE_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const PREVIOUS_MAX_FILE_BYTES: usize = 50 * 1024 * 1024;
/// Absolute PDF input limit, including explicitly reviewed sources.
pub(crate) const MAX_FILE_BYTES: usize = 500 * 1024 * 1024;
const MAX_TEXT_BYTES: usize = 128 * 1024;
const MAX_STREAM_BYTES: usize = 1024 * 1024;
const MAX_REPLY_BYTES: usize = 3 + 4 * MAX_PAGES + MAX_TEXT_BYTES;
const INPUT_CHUNK_BYTES: usize = 64 * 1024;
const PDF_HEADER: &[u8; 5] = b"%PDF-";
const SPOOL_DIRECTORY: &str = "KORTTY_PDF_SPOOL_DIRECTORY";
const TIMEOUT: Duration = Duration::from_secs(8);
const LARGE_TRANSFER_TIME: Duration = Duration::from_secs(60);
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
    InputLimit = 6,
}

impl Failure {
    fn message(self) -> &'static str {
        match self {
            Self::Malformed => "Invalid PDF or unsupported PDF text encoding",
            Self::Protected => "Encrypted or password-protected PDF",
            Self::NoText => "PDF has no extractable text layer",
            Self::Limit | Self::InputLimit => "PDF exceeds the extraction limits",
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

pub(crate) fn input_size_exceeded(error: &io::Error) -> bool {
    error.get_ref().and_then(|error| error.downcast_ref()) == Some(&Failure::InputLimit)
}

struct SourceBytes {
    digest: [u8; 32],
    len: usize,
}

pub(crate) struct ExtractedPdf {
    pub(crate) pages: Vec<String>,
    pub(crate) digest: [u8; 32],
    pub(crate) bytes: usize,
}

/// Streams an already opened source to the isolated worker and hashes exactly
/// the bytes that it received. The caller retains the source identity checks.
pub(crate) fn extract(
    file: &File,
    limit: u64,
    active: impl Fn() -> bool,
) -> io::Result<ExtractedPdf> {
    let deadline = Instant::now() + TIMEOUT;
    let limit = limit.min(MAX_FILE_BYTES as u64);
    if file.metadata()?.len() > limit {
        return Err(Failure::InputLimit.into());
    }
    let _permit = Permit::acquire(deadline, &active)?;
    #[cfg(test)]
    {
        // The test-harness executable has no desktop argument dispatcher. The real
        // process route is separately exercised against the production binary.
        if std::env::var_os("KORTTY_PDF_WORKER_EXE").is_none() {
            let mut bytes = Vec::new();
            let input = read_header(file, limit)?;
            let source = copy_source(input, &mut bytes, || check_active(deadline, &active))?;
            let pages = parse(&bytes)?;
            check_active(deadline, &active)?;
            return Ok(extracted(source, pages));
        }
    }
    let exe = std::env::current_exe()?;
    #[cfg(test)]
    let exe = std::env::var_os("KORTTY_PDF_WORKER_EXE").map_or(exe, std::path::PathBuf::from);
    run_worker(&exe, file.try_clone()?, limit, deadline, &active)
}

fn extracted(source: SourceBytes, pages: Vec<String>) -> ExtractedPdf {
    ExtractedPdf {
        pages,
        digest: source.digest,
        bytes: source.len,
    }
}

fn read_header<R: Read>(input: R, limit: u64) -> io::Result<io::Take<R>> {
    let mut input = input.take(limit + 1);
    let mut header = [0; PDF_HEADER.len()];
    let header_result = input.read_exact(&mut header);
    if input.limit() == 0 {
        return Err(Failure::InputLimit.into());
    }
    header_result.map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            Failure::Malformed.into()
        } else {
            error
        }
    })?;
    if &header != PDF_HEADER {
        return Err(Failure::Malformed.into());
    }
    Ok(input)
}

fn copy_source(
    mut input: io::Take<impl Read>,
    mut output: impl Write,
    check: impl Fn() -> io::Result<()>,
) -> io::Result<SourceBytes> {
    check()?;
    let initial_remaining = input.limit();
    let mut hash = Sha256::new();
    output.write_all(PDF_HEADER).map_err(|_| Failure::Worker)?;
    hash.update(PDF_HEADER);
    let mut buffer = vec![0; INPUT_CHUNK_BYTES];
    loop {
        check()?;
        let count = match input.read(&mut buffer) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            other => other?,
        };
        if count == 0 {
            break;
        }
        if input.limit() == 0 {
            return Err(Failure::InputLimit.into());
        }
        output
            .write_all(&buffer[..count])
            .map_err(|_| Failure::Worker)?;
        hash.update(&buffer[..count]);
    }
    Ok(SourceBytes {
        digest: hash.finalize().into(),
        len: PDF_HEADER.len() + (initial_remaining - input.limit()) as usize,
    })
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
    fn start(exe: &Path, spool_directory: Option<&Path>) -> io::Result<Self> {
        let mut command = Command::new(exe);
        command
            .arg(WORKER_ARGUMENT)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(directory) = spool_directory {
            command.env(SPOOL_DIRECTORY, directory);
        }
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
    file: impl Read + Send + 'static,
    limit: u64,
    deadline: Instant,
    active: &impl Fn() -> bool,
) -> io::Result<ExtractedPdf> {
    check_active(deadline, active)?;
    let source = read_header(file, limit)?;
    check_active(deadline, active)?;
    let large = limit > PREVIOUS_MAX_FILE_BYTES as u64;
    // The owner removes the staging directory after reaping the worker, even if
    // cancellation or a process limit terminates the worker midway through copying.
    let spool_directory = large
        .then(|| tempfile::Builder::new().prefix("kortty-pdf-").tempdir())
        .transpose()?;
    let mut worker = Worker::start(exe, spool_directory.as_ref().map(tempfile::TempDir::path))?;
    // On Windows the worker must be in its private Job Object before this writer sends bytes.
    let mut input = worker.child.stdin.take().ok_or(Failure::Worker)?;
    let output = worker.child.stdout.take().ok_or(Failure::Worker)?;
    let stopped = Arc::new(AtomicBool::new(false));
    let writing = Arc::clone(&stopped);
    let (written_at, receipt_time) = std::sync::mpsc::sync_channel(1);
    let transfer_deadline = if large {
        deadline + LARGE_TRANSFER_TIME
    } else {
        deadline
    };
    let writer = thread::Builder::new()
        .name("kortty-pdf-input".into())
        .spawn(move || {
            let receipt = copy_source(source, &mut input, || {
                check_active(transfer_deadline, &|| !writing.load(Ordering::Acquire))
            });
            let _ = written_at.send(Instant::now());
            drop(input);
            receipt
        })?;
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
            stopped.store(true, Ordering::Release);
            worker.stop();
            let _ = writer.join();
            return Err(error);
        }
    };
    let mut worker_deadline = transfer_deadline;
    let exited = loop {
        if large && let Ok(received) = receipt_time.try_recv() {
            worker_deadline = received + TIMEOUT;
        }
        if let Err(error) = check_active(worker_deadline, active) {
            stopped.store(true, Ordering::Release);
            worker.stop();
            break Err(error);
        }
        match worker.child.try_wait() {
            Ok(Some(status)) if status.success() => break Ok(()),
            Ok(Some(_)) => break Err(Failure::Worker.into()),
            Ok(None) => thread::sleep(POLL),
            Err(_) => {
                stopped.store(true, Ordering::Release);
                worker.stop();
                break Err(Failure::Worker.into());
            }
        }
    };
    let written = writer.join();
    let response = reader.join();
    exited?;
    let source = written.map_err(|_| Failure::Worker)??;
    check_active(worker_deadline, active)?;
    let pages = response.map_err(|_| Failure::Worker)??;
    Ok(extracted(source, pages))
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
    let input = io::stdin().lock();
    let result = if let Some(directory) = std::env::var_os(SPOOL_DIRECTORY) {
        kortty_files::with_spooled_input(input, Path::new(&directory), MAX_FILE_BYTES, parse)?
            .unwrap_or(Err(Failure::Limit))
    } else {
        let mut bytes = Vec::new();
        input
            .take((PREVIOUS_MAX_FILE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > PREVIOUS_MAX_FILE_BYTES {
            Err(Failure::Limit)
        } else {
            parse(&bytes)
        }
    };
    encode_reply(&mut io::stdout().lock(), result)
}

fn parse(bytes: &[u8]) -> Result<Vec<String>, Failure> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err(Failure::Limit);
    }
    if !bytes.starts_with(PDF_HEADER) {
        return Err(Failure::Malformed);
    }
    let mut options = LoadOptions::with_max_decompressed_size(MAX_STREAM_BYTES);
    options.strict = true;
    if bytes.len() > PREVIOUS_MAX_FILE_BYTES {
        options.filter = Some(text_objects);
    }
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

fn text_objects(id: ObjectId, object: &mut Object) -> Option<(ObjectId, Object)> {
    // Page text extraction does not read image XObjects or embedded attachments.
    // Retain page content and fonts while releasing these raw streams as they load.
    if let Object::Stream(stream) = object
        && (stream.dict.has_type(b"EmbeddedFile")
            || stream
                .dict
                .get(b"Subtype")
                .and_then(Object::as_name)
                .is_ok_and(|kind| kind == b"Image"))
    {
        return None;
    }
    Some((id, object.clone()))
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
