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

#[derive(Clone, Default)]
pub(crate) struct Cancel(Arc<AtomicBool>);

impl Cancel {
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

pub(super) fn capture(mut reader: impl Read) -> (Captured, bool) {
    let mut captured = Captured::default();
    let mut buffer = Zeroizing::new([0; 8192]);
    loop {
        match reader.read(&mut buffer[..]) {
            Ok(0) => return (captured, true),
            Ok(count) => captured.push(&buffer[..count]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return (captured, false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
