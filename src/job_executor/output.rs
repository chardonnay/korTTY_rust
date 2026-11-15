//! Bounded, separately masked stdout and stderr for the scheduler run history.

use std::fmt;
use std::io::{self, Read};
use std::time::{Duration, Instant};

use kortty_jobs::{MAX_RUN_OUTPUT_BYTES, RunOutput};
use zeroize::Zeroizing;

use crate::clipboard_policy::ClipboardPolicy;
use crate::scheduler_window::JournalDetail;
use crate::terminal_logging::{Policy, Redactor, Secrets};

use super::{Credentials, ExecutionPolicy};

const RAW_BYTES: usize = MAX_RUN_OUTPUT_BYTES * 2;
const LIMITED_UNITS: usize = 4_000;
const TRUNCATED: &str = "\n…[output truncated]";
const MASK_LIMIT: Duration = Duration::from_millis(250);

#[derive(Clone)]
pub(crate) struct CapturedOutput {
    streams: RunOutput,
    redactions: [u8; 32],
}

impl fmt::Debug for CapturedOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Captured scheduler output")
    }
}

impl CapturedOutput {
    pub(crate) fn current(&self, cached: Option<&ClipboardPolicy>) -> Option<&RunOutput> {
        let allowed = |policy: &ClipboardPolicy| {
            policy.automation_journal_allowed()
                && policy.recording_allowed
                && policy.terminal_logging.redaction_revision() == Some(self.redactions)
        };
        (cached.is_none_or(allowed) && allowed(&ClipboardPolicy::load())).then_some(&self.streams)
    }

    #[cfg(test)]
    pub(crate) fn fixture(streams: RunOutput, policy: &ClipboardPolicy) -> Self {
        Self {
            streams,
            redactions: policy.terminal_logging.redaction_revision().unwrap(),
        }
    }
}

pub(super) struct Scope {
    mode: JournalDetail,
    mask: Redactor,
    policy: Policy,
    redactions: [u8; 32],
}

impl Scope {
    pub(super) fn new<'a>(
        mode: JournalDetail,
        policy: &ExecutionPolicy,
        targets: usize,
        credentials: impl IntoIterator<Item = &'a Credentials>,
    ) -> Option<Self> {
        if mode == JournalDetail::Off || !policy.job_output_allowed {
            return None;
        }
        let current = ClipboardPolicy::load();
        let policy = &policy.job_output_redactions;
        if !capture_allowed(policy, &current) {
            return None;
        }
        let secrets = Secrets::scheduled_targets(targets);
        for credentials in credentials {
            credentials.add_journal_secrets(&secrets);
        }
        let mask = Redactor::for_journal(secrets.seal().ok()?, policy.clone());
        Some(Self {
            mode,
            mask,
            policy: policy.clone(),
            redactions: policy.redaction_revision()?,
        })
    }

    pub(super) fn authorized(&self) -> bool {
        capture_allowed(&self.policy, &ClipboardPolicy::load())
    }

    pub(super) fn finish(
        &self,
        [stdout, stderr]: [Stream; 2],
        interrupted: bool,
    ) -> Option<CapturedOutput> {
        if !self.authorized() {
            return None;
        }
        self.publish(RunOutput {
            stdout: self.mask_stream(stdout, interrupted),
            stderr: self.mask_stream(stderr, interrupted),
        })
    }

    fn mask_stream(&self, stream: Stream, interrupted: bool) -> String {
        let Stream {
            mut bytes,
            over_limit,
            complete,
        } = stream;
        let incomplete = over_limit || !complete || interrupted;
        let mut truncated = incomplete;
        if bytes.is_empty() {
            return String::new();
        }
        // A regex can span any number of lines and its result may depend on the
        // missing bytes. An unfinished stream cannot satisfy managed redactions.
        if incomplete && self.policy.has_redactions() {
            return self.clip(String::new(), true);
        }
        if incomplete && let Some(chunk) = bytes.utf8_chunks().last() {
            let trailing = chunk.invalid();
            if std::str::from_utf8(trailing).is_err_and(|error| error.error_len().is_none()) {
                let end = bytes.len() - trailing.len();
                bytes.truncate(end);
            }
        }
        let mut visible = VisibleText::default();
        vte::Parser::new().advance(&mut visible, &bytes);
        let deadline = Instant::now() + MASK_LIMIT;
        let Some((mut masked, _)) = self.mask.outbound_text(&visible.text, deadline) else {
            return String::new();
        };
        match self.mask.ends_with_partial_secret(&masked, deadline) {
            Some(true) => {
                let end = masked.rfind('\n').map_or(0, |last| last + 1);
                masked.truncate(end);
                truncated = true;
            }
            Some(false) => {}
            None => return String::new(),
        }
        let masked = std::mem::take(&mut *masked);
        if masked.trim().is_empty() && !truncated {
            return String::new();
        }
        self.clip(masked, truncated)
    }

    fn clip(&self, mut text: String, truncated: bool) -> String {
        let end = self.boundary(&text, 0);
        if truncated || end < text.len() {
            text.truncate(self.boundary(&text, self.marker_width()));
            text.push_str(TRUNCATED);
        }
        text
    }

    fn boundary(&self, text: &str, reserve: usize) -> usize {
        if self.mode == JournalDetail::Full {
            return text.floor_char_boundary(text.len().min(MAX_RUN_OUTPUT_BYTES - reserve));
        }
        let mut remaining = LIMITED_UNITS - reserve;
        text.char_indices()
            .find_map(|(index, character)| {
                let Some(next) = remaining.checked_sub(character.len_utf16()) else {
                    return Some(index);
                };
                remaining = next;
                None
            })
            .unwrap_or(text.len())
    }

    fn marker_width(&self) -> usize {
        if self.mode == JournalDetail::Full {
            TRUNCATED.len()
        } else {
            TRUNCATED.encode_utf16().count()
        }
    }

    fn publish(&self, mut streams: RunOutput) -> Option<CapturedOutput> {
        for stream in [&mut streams.stdout, &mut streams.stderr] {
            if self.mask.contains_known_secret(stream) {
                stream.clear();
            }
        }
        (!streams.stdout.is_empty() || !streams.stderr.is_empty()).then_some(CapturedOutput {
            streams,
            redactions: self.redactions,
        })
    }

    pub(super) fn append(&self, combined: &mut Combined, target: u64, captured: CapturedOutput) {
        if captured.redactions != self.redactions || !self.authorized() {
            return;
        }
        for (destination, stream) in combined
            .streams
            .iter_mut()
            .zip([captured.streams.stdout, captured.streams.stderr])
        {
            if destination.truncated || stream.is_empty() {
                continue;
            }
            if !destination.text.is_empty() {
                destination.text.push_str("\n\n");
            }
            use std::fmt::Write as _;
            let _ = writeln!(destination.text, "[Profile {target}]");
            destination.text.push_str(stream.trim_end_matches('\n'));
            let end = self.boundary(&destination.text, 0);
            if end < destination.text.len() {
                destination
                    .text
                    .truncate(self.boundary(&destination.text, self.marker_width()));
                destination.truncated = true;
            }
        }
    }

    pub(super) fn finish_combined(&self, combined: Combined) -> Option<CapturedOutput> {
        if !self.authorized() {
            return None;
        }
        let [stdout, stderr] = combined
            .streams
            .map(|stream| self.clip(stream.text, stream.truncated));
        self.publish(RunOutput { stdout, stderr })
    }
}

#[derive(Default)]
struct VisibleText {
    text: Zeroizing<String>,
    skip_ss3: bool,
}

impl vte::Perform for VisibleText {
    fn print(&mut self, character: char) {
        if !std::mem::take(&mut self.skip_ss3)
            && crate::session_journal::is_display_character(character)
        {
            self.text.push(character);
        }
    }

    fn execute(&mut self, byte: u8) {
        self.skip_ss3 = false;
        match byte {
            b'\n' | b'\t' => self.text.push(char::from(byte)),
            0x08 => {
                self.text.pop();
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _: bool, byte: u8) {
        self.skip_ss3 = intermediates.is_empty() && byte == b'O';
    }
}

fn capture_allowed(reviewed: &Policy, current: &ClipboardPolicy) -> bool {
    current.automation_journal_allowed()
        && current.recording_allowed
        && reviewed.equivalent_redactions(&current.terminal_logging)
}

#[derive(Default)]
pub(super) struct Stream {
    bytes: Zeroizing<Vec<u8>>,
    over_limit: bool,
    complete: bool,
}

pub(super) struct Reader<'a, R> {
    inner: R,
    stream: Option<&'a mut Stream>,
}

impl<'a, R> Reader<'a, R> {
    pub(super) fn new(inner: R, stream: Option<&'a mut Stream>) -> Self {
        Self { inner, stream }
    }

    pub(super) fn finish(&mut self, complete: bool) {
        if let Some(stream) = &mut self.stream {
            stream.complete = complete;
        }
    }
}

impl<R: Read> Read for Reader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let count = self.inner.read(buf)?;
        if let Some(stream) = self.stream.as_mut() {
            let retained = count.min(RAW_BYTES - stream.bytes.len());
            stream.bytes.extend_from_slice(&buf[..retained]);
            stream.over_limit |= retained < count;
        }
        Ok(count)
    }
}

#[derive(Default)]
pub(super) struct Combined {
    streams: [CombinedStream; 2],
}

#[derive(Default)]
struct CombinedStream {
    text: String,
    truncated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn scope(mode: JournalDetail, policy: Policy, known: &[&str]) -> Scope {
        let secrets = Secrets::default();
        for secret in known {
            secrets.add(secret);
        }
        Scope {
            mode,
            mask: Redactor::for_journal(secrets.seal().unwrap(), policy.clone()),
            redactions: policy.redaction_revision().unwrap(),
            policy,
        }
    }

    fn capture(bytes: &[u8], complete: bool) -> Stream {
        let mut stream = Stream::default();
        let mut reader = Reader::new(Cursor::new(bytes), Some(&mut stream));
        let mut chunk = [0; 7];
        let mut drained = 0;
        loop {
            let count = reader.read(&mut chunk).unwrap();
            if count == 0 {
                break;
            }
            drained += count;
        }
        reader.finish(complete);
        assert_eq!(drained, bytes.len());
        stream
    }

    #[test]
    fn scheduler_output_masks_credentials_crossing_the_visible_prefix_and_drains_after_limit() {
        let scope = scope(
            JournalDetail::LimitedRedacted,
            Policy::default(),
            &["vault-password", "jump-passphrase", "sudo-password"],
        );
        let prefix = "🧪".repeat((LIMITED_UNITS - scope.marker_width() - 1) / 2);
        let text = format!(
            "{prefix}sudo-password {}{}",
            "q".repeat(RAW_BYTES),
            "unretained tail"
        );
        let captured = capture(text.as_bytes(), true);
        assert_eq!(captured.bytes.len(), RAW_BYTES);
        let shown = scope.mask_stream(captured, false);
        assert!(shown.starts_with(&prefix));
        assert!(shown.ends_with(TRUNCATED));
        assert!(shown.encode_utf16().count() <= LIMITED_UNITS);
        assert!(!shown.contains("sudo") && !shown.contains("unretained"));
        assert_eq!(
            scope.mask_stream(
                capture(b"vault-password\r\n\tjump-passphrase\x07", true),
                false
            ),
            "***\n\t***"
        );
    }

    #[test]
    fn scheduler_output_withholds_incomplete_utf8_and_known_secret_fragments() {
        let scope = scope(
            JournalDetail::Full,
            Policy::default(),
            &["private-credential"],
        );
        let mut raw = b"Safe first line\nprivate-creden".to_vec();
        raw.extend_from_slice(&[0xf0, 0x9f]);
        let shown = scope.mask_stream(capture(&raw, false), true);
        assert!(shown.starts_with("Safe first line\n"));
        assert!(shown.ends_with(TRUNCATED));
        assert!(!shown.contains("private-creden") && !shown.contains('\u{fffd}'));
        let visible =
            scope.mask_stream(capture("界".repeat(RAW_BYTES / 2).as_bytes(), true), false);
        assert!(visible.len() <= MAX_RUN_OUTPUT_BYTES && visible.ends_with(TRUNCATED));
        assert_eq!(scope.mask_stream(capture(b" \r\n\t", true), false), "");
    }

    #[test]
    fn scheduler_output_masks_secrets_split_by_terminal_controls() {
        let scope = scope(
            JournalDetail::Full,
            Policy::default(),
            &["private-credential"],
        );
        let decorated = b"private-\x1b[31mcre\x1b]0;hidden title\x07dential\x1b[0m\n";
        assert_eq!(scope.mask_stream(capture(decorated, true), false), "***\n");
        let incomplete = b"Safe first line\nprivate-creden\x1b]0;incomplete";
        let shown = scope.mask_stream(capture(incomplete, false), true);
        assert!(shown.starts_with("Safe first line\n") && shown.ends_with(TRUNCATED));
        assert!(!shown.contains("private") && !shown.contains("incomplete"));
    }

    #[test]
    fn scheduler_output_requires_managed_capture_and_matching_mask_authority() {
        let policy = ClipboardPolicy::default();
        let denied = crate::clipboard_policy::parse(
            "[meta]\nschema-version = 1\n[[rule]]\n[rule.security]\nallow-terminal-recording = false\n",
        )
        .unwrap();
        assert!(capture_allowed(&policy.terminal_logging, &policy));
        assert!(!capture_allowed(&policy.terminal_logging, &denied));
        let mut execution = ExecutionPolicy::unrestricted(true);
        execution.job_output_allowed = false;
        assert!(Scope::new(JournalDetail::Full, &execution, 0, None).is_none());
        execution.job_output_allowed = true;
        assert!(Scope::new(JournalDetail::Off, &execution, 0, None).is_none());

        let proof = CapturedOutput::fixture(
            RunOutput {
                stdout: "fixture output".into(),
                stderr: String::new(),
            },
            &policy,
        );
        assert!(proof.current(Some(&denied)).is_none());
        assert!(!format!("{proof:?}").contains("fixture output"));
    }
}
