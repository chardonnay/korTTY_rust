//! One-use input for credentials on a dedicated SSH exec channel.

use std::fmt;
use std::io;
use std::sync::Arc;

use zeroize::Zeroizing;

use super::{Command, InputBytes, InputPermission, SshAbort, SshWriter, closed};

const MAX_SENSITIVE_LINE_BYTES: usize = 1025;
const MAX_SENSITIVE_FRAME_BYTES: usize = 1028;
const MAX_COMMIT_MARKER_BYTES: usize = 64;

/// A single UTF-8 line whose owned bytes are cleared when the line is dropped.
pub struct SensitiveLine(Zeroizing<Vec<u8>>);

impl SensitiveLine {
    /// Validates the secret and appends the only permitted line terminator.
    pub fn new(secret: &str) -> io::Result<Self> {
        if !valid_secret(secret) {
            return Err(invalid_line());
        }
        let mut line = Zeroizing::new(Vec::with_capacity(secret.len() + 1));
        line.extend_from_slice(secret.as_bytes());
        line.push(b'\n');
        Ok(Self(line))
    }

    /// Writes a length-prefixed line to an owned credential transport.
    pub fn write_wire(&self, writer: &mut impl io::Write) -> io::Result<()> {
        writer.write_all(&(self.0.len() as u32).to_be_bytes())?;
        writer.write_all(&self.0)
    }

    /// Reads one length-prefixed line directly into zeroizing storage.
    pub fn read_wire(reader: &mut impl io::Read) -> io::Result<Self> {
        let mut length = [0; 4];
        reader.read_exact(&mut length)?;
        let length = u32::from_be_bytes(length) as usize;
        if !(2..=MAX_SENSITIVE_LINE_BYTES).contains(&length) {
            return Err(invalid_line());
        }
        let mut line = Zeroizing::new(vec![0; length]);
        reader.read_exact(&mut line)?;
        if !line
            .strip_suffix(b"\n")
            .and_then(|secret| std::str::from_utf8(secret).ok())
            .is_some_and(valid_secret)
        {
            return Err(invalid_line());
        }
        Ok(Self(line))
    }
}

impl fmt::Debug for SensitiveLine {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Sensitive SSH input")
    }
}

fn valid_secret(secret: &str) -> bool {
    !secret.is_empty()
        && secret.len() < MAX_SENSITIVE_LINE_BYTES
        && !secret.chars().any(char::is_control)
}

fn invalid_line() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "Invalid sensitive SSH input")
}

impl SshWriter {
    /// Sends a bounded secret frame and a separately guarded protocol commit marker.
    /// The remote helper must require the marker because closing a channel can
    /// also produce EOF on the helper's input.
    pub fn send_sensitive_input(
        mut self,
        input: Zeroizing<Vec<u8>>,
        commit: &'static [u8],
        permitted: impl Fn() -> bool + Send + Sync + 'static,
    ) -> io::Result<()> {
        if self.input_closed {
            return Err(closed());
        }
        let result = if !(1..=MAX_SENSITIVE_FRAME_BYTES).contains(&input.len())
            || !(1..=MAX_COMMIT_MARKER_BYTES).contains(&commit.len())
        {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid protected SSH input frame or commit marker",
            ))
        } else {
            let permission: InputPermission = Arc::new(permitted);
            self.queue(Command::Data(InputBytes::Sensitive(
                input,
                Some(Arc::clone(&permission)),
            )))
            .and_then(|()| {
                self.queue(Command::Data(InputBytes::Sensitive(
                    Zeroizing::new(commit.to_vec()),
                    Some(permission),
                )))
            })
            .and_then(|()| self.close_input())
        };
        if result.is_err() {
            SshAbort(self.shared.clone()).abort();
        }
        result
    }
}

/// Consumes the only input handle when sending a line or sending EOF alone.
pub struct SensitiveExecInput(SshWriter);

impl SensitiveExecInput {
    pub(super) fn new(writer: SshWriter) -> Self {
        Self(writer)
    }

    pub fn send(self, line: SensitiveLine) -> io::Result<()> {
        self.send_guarded(line, None)
    }

    /// Checks permission on the SSH worker before each write of the line.
    pub fn send_if(
        self,
        line: SensitiveLine,
        permitted: impl Fn() -> bool + Send + Sync + 'static,
    ) -> io::Result<()> {
        self.send_guarded(line, Some(Arc::new(permitted)))
    }

    /// Queues one checked credential line and hands the writer to an owned
    /// framed protocol. Wait for the authenticated helper's greeting before
    /// writing any protocol data.
    pub fn send_then(
        self,
        line: SensitiveLine,
        permitted: impl Fn() -> bool + Send + Sync + 'static,
    ) -> io::Result<SshWriter> {
        match self.0.queue(Command::Data(InputBytes::Sensitive(
            line.0,
            Some(Arc::new(permitted)),
        ))) {
            Ok(()) => Ok(self.0),
            Err(error) => {
                SshAbort(self.0.shared.clone()).abort();
                Err(error)
            }
        }
    }

    fn send_guarded(
        mut self,
        line: SensitiveLine,
        permitted: Option<InputPermission>,
    ) -> io::Result<()> {
        let result = self
            .0
            .queue(Command::Data(InputBytes::Sensitive(line.0, permitted)))
            .and_then(|()| self.0.close_input());
        if result.is_err() {
            SshAbort(self.0.shared.clone()).abort();
        }
        result
    }

    pub fn close(mut self) -> io::Result<()> {
        self.0.close_input()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, mpsc};
    use std::thread;
    use std::time::Duration;

    use mio::{Poll, Waker};

    use super::super::{CONTROL, Shared};
    use super::*;

    #[test]
    fn sensitive_line_accepts_only_one_bounded_utf8_line() {
        let line = SensitiveLine::new("fïxture 東京").unwrap();
        assert_eq!(line.0.as_slice(), "fïxture 東京\n".as_bytes());
        assert_eq!(format!("{line:?}"), "Sensitive SSH input");
        for rejected in ["", "line\nbreak", "line\rbreak", "abc\0"] {
            assert!(SensitiveLine::new(rejected).is_err());
        }
        assert!(SensitiveLine::new(&"x".repeat(MAX_SENSITIVE_LINE_BYTES)).is_err());
    }

    #[test]
    fn sensitive_wire_rejects_oversized_truncated_or_multiline_payloads() {
        let line = SensitiveLine::new("sëcret 東京").unwrap();
        let mut wire = Zeroizing::new(Vec::new());
        line.write_wire(&mut *wire).unwrap();
        assert_eq!(
            SensitiveLine::read_wire(&mut wire.as_slice())
                .unwrap()
                .0
                .as_slice(),
            line.0.as_slice()
        );
        assert!(SensitiveLine::read_wire(&mut &wire[..wire.len() - 1]).is_err());
        assert!(SensitiveLine::read_wire(&mut &[0, 0, 4, 2][..]).is_err());
        assert!(SensitiveLine::read_wire(&mut &b"\0\0\0\x04x\ny\n"[..]).is_err());
    }

    #[test]
    fn sensitive_exec_input_moves_the_secret_before_eof() {
        let poll = Poll::new().unwrap();
        let wake = Arc::new(Waker::new(poll.registry(), CONTROL).unwrap());
        let (input, commands) = mpsc::sync_channel(2);
        let writer = SensitiveExecInput::new(SshWriter {
            shared: Shared::new(wake),
            input,
            input_closed: false,
        });
        thread::scope(|scope| {
            let reader = scope.spawn(move || {
                let wait = Duration::from_secs(2);
                let Ok(Command::Data(InputBytes::Sensitive(line, None))) =
                    commands.recv_timeout(wait)
                else {
                    panic!("expected an owned sensitive line");
                };
                assert_eq!(line.as_slice(), b"fixture\n");
                let Ok(Command::Eof(ack)) = commands.recv_timeout(wait) else {
                    panic!("expected SSH EOF after the line");
                };
                ack.send(()).unwrap();
            });
            writer.send(SensitiveLine::new("fixture").unwrap()).unwrap();
            reader.join().unwrap();
        });
    }

    #[test]
    fn sensitive_exec_input_continues_with_owned_framed_data() {
        use std::io::Write as _;

        let poll = Poll::new().unwrap();
        let wake = Arc::new(Waker::new(poll.registry(), CONTROL).unwrap());
        let (input, commands) = mpsc::sync_channel(2);
        let writer = SensitiveExecInput::new(SshWriter {
            shared: Shared::new(wake),
            input,
            input_closed: false,
        });
        let mut writer = writer
            .send_then(SensitiveLine::new("fixture").unwrap(), || true)
            .unwrap();
        let Ok(Command::Data(secret @ InputBytes::Sensitive(_, _))) = commands.try_recv() else {
            panic!("expected an owned credential before protocol data");
        };
        assert!(secret.permitted());
        assert_eq!(secret.as_ref(), b"fixture\n");
        assert!(matches!(
            commands.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        writer.write_all(b"frame").unwrap();
        assert!(
            matches!(commands.try_recv(), Ok(Command::Data(InputBytes::Stream(bytes))) if bytes == b"frame")
        );
    }

    #[test]
    fn sensitive_frame_checks_the_commit_marker_separately_from_the_body() {
        use std::sync::atomic::{AtomicBool, Ordering};

        const FRAME: &[u8] = b"\0\0\0\x04p\nw\r";
        const COMMIT: &[u8] = b"fixture-commit\n";
        let poll = Poll::new().unwrap();
        let wake = Arc::new(Waker::new(poll.registry(), CONTROL).unwrap());
        let (input, commands) = mpsc::sync_channel(3);
        let writer = SshWriter {
            shared: Shared::new(wake),
            input,
            input_closed: false,
        };
        let permission = Arc::new(AtomicBool::new(true));
        let guard = Arc::clone(&permission);
        thread::scope(|scope| {
            let worker = scope.spawn(move || {
                let wait = Duration::from_secs(2);
                let Ok(Command::Data(body @ InputBytes::Sensitive(..))) =
                    commands.recv_timeout(wait)
                else {
                    panic!("expected the owned secret bytes");
                };
                assert!(body.permitted());
                assert_eq!(body.as_ref(), FRAME);
                permission.store(false, Ordering::SeqCst);
                let Ok(Command::Data(marker @ InputBytes::Sensitive(..))) =
                    commands.recv_timeout(wait)
                else {
                    panic!("expected the separately guarded marker after the body");
                };
                assert_eq!(marker.as_ref(), COMMIT);
                assert!(!marker.permitted());
                // The pump must close here without sending the marker or EOF.
            });
            let error = writer
                .send_sensitive_input(Zeroizing::new(FRAME.to_vec()), COMMIT, move || {
                    guard.load(Ordering::SeqCst)
                })
                .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
            worker.join().unwrap();
        });
    }
}
