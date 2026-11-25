//! Bounds and process results shared by native agent command runners.

use std::fmt;
use std::io::{self, Read};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use zeroize::Zeroizing;

pub(super) const COMMAND_LIMIT: Duration = Duration::from_secs(15 * 60);
pub(super) const POLL: Duration = Duration::from_millis(35);
const OUTPUT_LIMIT: usize = 64 * 1024;
// Lossy UTF-8 can produce three bytes per input byte. Allow both streams
// and their separator when masking the retained output together.
pub(crate) const CAPTURED_TEXT_LIMIT: usize = 6 * OUTPUT_LIMIT + 1;

#[derive(Clone, Default)]
pub(crate) struct Cancel(Arc<AtomicBool>, Option<OutputObserver>);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OutputStream {
    Stdout,
    Stderr,
}

impl OutputStream {
    pub(crate) fn index(self) -> usize {
        match self {
            Self::Stdout => 0,
            Self::Stderr => 1,
        }
    }
}

type OutputObserver = Arc<dyn Fn(OutputStream, &Captured, bool) + Send + Sync>;

impl Cancel {
    // A command copy shares cancellation but owns its optional diagnostic scope.
    pub(crate) fn with_output(
        &self,
        observer: impl Fn(OutputStream, &Captured, bool) + Send + Sync + 'static,
    ) -> Self {
        Self(Arc::clone(&self.0), Some(Arc::new(observer)))
    }

    pub(super) fn output(&self, stream: OutputStream, captured: &Captured, finished: bool) {
        if !self.requested()
            && let Some(observer) = &self.1
        {
            observer(stream, captured, finished);
        }
    }

    pub(crate) fn request(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub(crate) fn requested(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Status {
    Exited(u32),
    Cancelled,
    TimedOut,
    TransportFailed,
    MissingExitCode,
}

pub(crate) struct Outcome {
    pub(crate) status: Status,
    pub(crate) stdout: Captured,
    pub(crate) stderr: Captured,
}

#[cfg(test)]
impl Outcome {
    pub(crate) fn successful_probe_fixture(stdout: &str) -> Self {
        let mut captured = Captured::default();
        captured.push(stdout.as_bytes());
        Self {
            status: Status::Exited(0),
            stdout: captured,
            stderr: Captured::default(),
        }
    }
}

impl fmt::Debug for Outcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Agent command result")
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
pub(crate) struct Captured {
    bytes: Zeroizing<Vec<u8>>,
    truncated: bool,
    #[cfg(windows)]
    oem_fallback: bool,
}

impl Captured {
    pub(super) fn push(&mut self, bytes: &[u8]) {
        let retained = bytes.len().min(OUTPUT_LIMIT - self.bytes.len());
        self.bytes.extend_from_slice(&bytes[..retained]);
        self.truncated |= retained < bytes.len();
    }

    pub(crate) fn progress_revision(&self) -> usize {
        self.bytes.len() * 2 + usize::from(self.truncated)
    }

    pub(crate) fn text(&self) -> Zeroizing<String> {
        // A byte limit may cut through a credential or a UTF-8 character.
        // Keep complete lines before the cutoff so a partial value is not shown.
        let end = if self.truncated {
            self.bytes
                .iter()
                .rposition(|&byte| byte == b'\n')
                .map_or(0, |end| end + 1)
        } else {
            self.bytes.len()
        };
        let bytes = &self.bytes[..end];
        #[cfg(windows)]
        if self.oem_fallback {
            let mut output = Zeroizing::new(String::with_capacity(bytes.len()));
            // CMD built-ins can emit OEM text while external tools may emit UTF-8.
            // Preserve valid UTF-8 lines and use the native OEM page otherwise.
            for line in bytes.split_inclusive(|&byte| byte == b'\n') {
                match std::str::from_utf8(line) {
                    Ok(text) => output.push_str(text),
                    Err(_) => match kortty_pty::decode_oem_output(line) {
                        Ok(text) => output.push_str(&Zeroizing::new(text)),
                        Err(_) => output.push_str(&String::from_utf8_lossy(line)),
                    },
                }
            }
            return output;
        }
        Zeroizing::new(String::from_utf8_lossy(bytes).into_owned())
    }

    #[cfg(windows)]
    pub(super) fn use_oem_fallback(&mut self) {
        self.oem_fallback = true;
    }

    pub(crate) fn truncated(&self) -> bool {
        self.truncated
    }
}

pub(super) fn capture_observed(
    mut reader: impl Read,
    cancel: &Cancel,
    stream: OutputStream,
    _oem: bool,
) -> (Captured, bool) {
    let mut captured = Captured::default();
    #[cfg(windows)]
    if _oem {
        captured.use_oem_fallback();
    }
    let mut buffer = Zeroizing::new([0; 8192]);
    let complete = loop {
        match reader.read(&mut buffer[..]) {
            Ok(0) => break true,
            Ok(count) => {
                captured.push(&buffer[..count]);
                cancel.output(stream, &captured, false);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break false,
        }
    };
    cancel.output(stream, &captured, true);
    (captured, complete)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observed_capture_shares_cancellation_without_changing_capture() {
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = events.clone();
        let base = Cancel::default();
        let observed = base.with_output(move |stream, captured, finished| {
            sink.lock()
                .unwrap()
                .push((stream, captured.text().to_string(), finished));
        });
        let (captured, complete) = capture_observed(
            &b"fixture stdout\n"[..],
            &observed,
            OutputStream::Stdout,
            false,
        );
        assert!(complete);
        assert_eq!(&*captured.text(), "fixture stdout\n");
        let values = events.lock().unwrap();
        assert_eq!(values.len(), 2);
        assert_eq!(
            values[0],
            (OutputStream::Stdout, "fixture stdout\n".into(), false)
        );
        assert!(values[1].2);
        drop(values);
        base.request();
        assert!(observed.requested());
        let _ = capture_observed(&b"late output"[..], &observed, OutputStream::Stderr, false);
        assert_eq!(events.lock().unwrap().len(), 2);
    }

    #[test]
    fn command_capture_bounds_output_and_discards_partial_lines_at_cutoff() {
        let mut output = Captured::default();
        output.push(b"complete line\n");
        output.push(&vec![b'x'; OUTPUT_LIMIT]);
        output.push(b"\nDiscarded");
        assert_eq!(output.bytes.len(), OUTPUT_LIMIT);
        assert!(output.truncated());
        assert_eq!(&*output.text(), "complete line\n");
    }

    #[cfg(windows)]
    #[test]
    fn command_capture_decodes_utf8_and_oem_lines_independently() {
        let oem = b"native \x82\n";
        let decoded = kortty_pty::decode_oem_output(oem).unwrap();
        let mut capture = Captured::default();
        capture.use_oem_fallback();
        capture.push("UTF-8 東京\n".as_bytes());
        capture.push(oem);
        assert_eq!(&*capture.text(), &format!("UTF-8 東京\n{decoded}"));
    }
}
