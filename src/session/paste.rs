use std::io;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, Thread};
use std::time::{Duration, Instant};

use super::{InputError, MAX_INPUT_BYTES, Session, StreamInput};

const BEGIN: &[u8] = b"\x1b[200~";
const END: &[u8] = b"\x1b[201~";

#[derive(Default)]
pub(super) struct Cancellation {
    requested: AtomicBool,
    writer: OnceLock<Thread>,
}

impl Cancellation {
    fn cancel(&self) {
        self.requested.store(true, Ordering::Release);
        if let Some(writer) = self.writer.get() {
            writer.unpark();
        }
    }

    fn requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }
}

pub(super) struct Pacing {
    pub(super) hp2392: bool,
    delay: Duration,
    cancellation: Arc<Cancellation>,
}

fn paste_body(bytes: &[u8]) -> (&[u8], bool) {
    bytes
        .strip_prefix(BEGIN)
        .and_then(|body| body.strip_suffix(END))
        .map_or((bytes, false), |body| (body, true))
}

fn paste_lines(body: &[u8], bracketed: bool) -> impl Iterator<Item = &[u8]> {
    let delimiter = if bracketed { b'\n' } else { b'\r' };
    body.split_inclusive(move |byte| *byte == delimiter)
}

impl Pacing {
    /// The session's sole writer keeps other input out of an open bracketed paste.
    pub(super) fn write(
        &self,
        bytes: &[u8],
        closed: impl Fn() -> bool,
        mut write_line: impl FnMut(&[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        let _ = self.cancellation.writer.set(thread::current());
        let (body, bracketed) = paste_body(bytes);
        let mut sent = false;
        for line in paste_lines(body, bracketed) {
            if sent {
                let deadline = Instant::now() + self.delay;
                while !closed() && !self.cancellation.requested() {
                    let Some(wait) = deadline.checked_duration_since(Instant::now()) else {
                        break;
                    };
                    thread::park_timeout(wait);
                }
            }
            if closed() || self.cancellation.requested() {
                break;
            }
            if !sent && bracketed {
                let mut first = Vec::with_capacity(BEGIN.len() + line.len());
                first.extend_from_slice(BEGIN);
                first.extend_from_slice(line);
                write_line(&first)?;
            } else {
                write_line(line)?;
            }
            sent = true;
        }
        if bracketed && sent && !closed() {
            write_line(END)?;
        }
        Ok(())
    }
}

impl Session {
    pub(crate) fn is_pacing_paste(&self) -> bool {
        self.shared
            .paste_cancellation
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .upgrade()
            .is_some()
    }

    pub(crate) fn cancel_paced_paste(&self) -> bool {
        let cancellation = self
            .shared
            .paste_cancellation
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .upgrade();
        if let Some(cancellation) = cancellation {
            cancellation.cancel();
            return true;
        }
        false
    }

    pub(crate) fn send_paste_with_delay(
        &self,
        bytes: Vec<u8>,
        delay_ms: u16,
    ) -> Result<(), InputError> {
        let (body, bracketed) = paste_body(&bytes);
        if delay_ms == 0 || paste_lines(body, bracketed).nth(1).is_none() {
            return self.send_paste(bytes);
        }
        if bytes.len() > MAX_INPUT_BYTES {
            return Err(InputError::TooLarge);
        }
        let mut pending = self
            .shared
            .paste_cancellation
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if pending.upgrade().is_some() {
            return Err(InputError::Busy);
        }
        let cancellation = Arc::new(Cancellation::default());
        *pending = Arc::downgrade(&cancellation);
        self.send_stream(StreamInput::PacedPaste {
            bytes,
            clear_typeahead: self.typeahead_request().enabled,
            pacing: Pacing {
                hp2392: self.frame().screen.profile == kortty_terminal::TerminalProfile::Hp2392,
                delay: Duration::from_millis(u64::from(delay_ms)),
                cancellation,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paced_paste_keeps_line_endings_and_one_bracketed_block() {
        let pacing = Pacing {
            hp2392: false,
            delay: Duration::from_millis(5),
            cancellation: Arc::default(),
        };
        let mut writes = Vec::new();
        pacing
            .write(
                b"\x1b[200~one\ntwo\n\x1b[201~",
                || false,
                |line| {
                    writes.push((Instant::now(), line.to_vec()));
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(writes.len(), 3);
        assert_eq!(writes[0].1, b"\x1b[200~one\n");
        assert_eq!(writes[1].1, b"two\n");
        assert_eq!(writes[2].1, END);
        assert!(writes[1].0.duration_since(writes[0].0) >= pacing.delay);
    }

    #[test]
    fn cancelling_a_paste_closes_only_the_original_open_block() {
        for (bytes, expected) in [
            (
                b"\x1b[200~one\ntwo\x1b[201~".as_slice(),
                b"\x1b[200~one\n\x1b[201~".as_slice(),
            ),
            (b"one\rtwo", b"one\r"),
        ] {
            let pacing = Pacing {
                hp2392: false,
                delay: Duration::from_millis(1000),
                cancellation: Arc::default(),
            };
            let mut sent = Vec::new();
            pacing
                .write(
                    bytes,
                    || false,
                    |line| {
                        sent.extend_from_slice(line);
                        pacing.cancellation.cancel();
                        Ok(())
                    },
                )
                .unwrap();
            assert_eq!(sent, expected);
        }
    }
}
