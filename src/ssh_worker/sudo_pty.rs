//! Run-scoped SSH terminal protocol. Validation finishes before command input is enabled.

use std::io;
use std::time::{Duration, Instant};

use kortty_ssh::{ExecLease, SensitiveLine, SshReader, SshWriter};
use zeroize::{Zeroize, Zeroizing};

use super::password::{self, Answer, Prompt, Status};

const POLL: Duration = Duration::from_millis(35);
const CHALLENGE_LIMIT: Duration = Duration::from_secs(30);
const ANSWER_LIMIT: Duration = Duration::from_secs(5 * 60);

const SCRIPT: &str = r#"
set +x
if ! [ -t 0 ] || ! [ -t 1 ]; then
    printf '\036%s:124\n' "$2"
    exit 124
fi
if ! /bin/stty raw -echo -echonl min 1 time 0; then
    printf '\036%s:125\n' "$2"
    exit 125
fi
printf '%s\n' "$3"
/usr/bin/sudo -S -p "$1" -v -u root
status=$?
if [ "$status" -ne 0 ]; then
    printf '\036%s:%s\n' "$2" "$status"
    exit "$status"
fi
if ! /bin/stty raw -echo -echonl min 1 time 0; then
    printf '\036%s:125\n' "$2"
    exit 125
fi
printf '\036%s:0\n' "$2"
shift 3
while IFS=' ' read -r request encoded; do
    command=$(printf '%b.' "$encoded")
    /bin/sh -c "${command%.}" </dev/null
    status=$?
    if ! /bin/stty raw -echo -echonl min 1 time 0; then
        exit 125
    fi
    printf '\036%s:%s\n' "$request" "$status"
done
"#;

pub fn token(nonce: [u8; 16]) -> String {
    format!("{:032x}", u128::from_be_bytes(nonce))
}

pub fn command(nonce: [u8; 16]) -> String {
    format!(
        "exec /bin/sh -c '{}' kortty-sudo '{}' {} '{}'",
        SCRIPT.replace('\'', "'\\''"),
        password::marker(nonce),
        token(nonce),
        armed_marker(nonce)
    )
}

fn armed_marker(nonce: [u8; 16]) -> String {
    format!("KORTTY_SUDO_ARMED_{}", token(nonce))
}

pub struct Authenticated {
    pub reader: SshReader,
    pub writer: SshWriter,
}

/// The password source is used only while the fixed validation script owns stdin.
pub fn authenticate(
    lease: &mut ExecLease,
    nonce: [u8; 16],
    permitted: impl Fn() -> bool + Clone + Send + Sync + 'static,
    mut answer: impl FnMut() -> Answer,
    challenge: impl FnOnce() -> bool,
) -> Result<Authenticated, Status> {
    let mut reader = lease.take_reader().ok_or(Status::TransportFailed)?;
    let marker = password::marker(nonce);
    let mut prompt = Prompt::new(&marker);
    let mut status = Reply::new(&token(nonce));
    let mut armed = Marker::new(&armed_marker(nonce));
    let mut buffer = Zeroizing::new([0; 8192]);
    let mut deadline = Instant::now() + CHALLENGE_LIMIT;
    let mut challenge = Some(challenge);
    let mut writer = None;
    loop {
        if !permitted() {
            return Err(Status::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(Status::TimedOut);
        }
        match reader.read_timeout(&mut buffer[..], POLL) {
            Ok(0) => {
                return Err(lease
                    .exit_code()
                    .map_or(Status::MissingExitCode, Status::Exited));
            }
            Ok(count) => {
                let completed = status.observe(&buffer[..count]);
                let prompted = armed
                    .after(&buffer[..count])
                    .is_some_and(|bytes| prompt.observe(bytes));
                buffer[..count].zeroize();
                if let Some(code) = completed.map_err(|_| Status::TransportFailed)? {
                    if code != 0 {
                        return Err(Status::Exited(code));
                    }
                    if !armed.found() || (challenge.is_none() && writer.is_none()) {
                        return Err(Status::TransportFailed);
                    }
                    let writer = writer
                        .or_else(|| lease.take_writer())
                        .ok_or(Status::TransportFailed)?;
                    return Ok(Authenticated { reader, writer });
                }
                if prompted {
                    let notify = challenge.take().ok_or(Status::Exited(1))?;
                    if !notify() {
                        return Err(Status::Cancelled);
                    }
                    deadline = Instant::now() + ANSWER_LIMIT;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
                ) => {}
            Err(_) => return Err(Status::TransportFailed),
        }
        if writer.is_none() {
            match answer() {
                Answer::Line(line) if challenge.is_none() && permitted() => {
                    let gate = permitted.clone();
                    writer = Some(submit(lease, line, move || {
                        Instant::now() < deadline && gate()
                    })?);
                }
                Answer::Line(_) | Answer::Closed => return Err(Status::Cancelled),
                Answer::Pending => {}
            }
        }
    }
}

fn submit(
    lease: &mut ExecLease,
    line: SensitiveLine,
    permitted: impl Fn() -> bool + Send + Sync + 'static,
) -> Result<SshWriter, Status> {
    lease
        .take_sensitive_exec_input()
        .ok_or(Status::TransportFailed)?
        .send_then(line, permitted)
        .map_err(|_| Status::TransportFailed)
}

struct Marker {
    bytes: Vec<u8>,
    matched: usize,
}

impl Marker {
    fn new(marker: &str) -> Self {
        Self {
            bytes: format!("{marker}\n").into_bytes(),
            matched: 0,
        }
    }

    fn found(&self) -> bool {
        self.matched == self.bytes.len()
    }

    fn after<'a>(&mut self, bytes: &'a [u8]) -> Option<&'a [u8]> {
        if self.found() {
            return Some(bytes);
        }
        for (index, &byte) in bytes.iter().enumerate() {
            if byte == self.bytes[self.matched] {
                self.matched += 1;
                if self.found() {
                    return Some(&bytes[index + 1..]);
                }
            } else {
                self.matched = usize::from(byte == self.bytes[0]);
            }
        }
        None
    }
}

/// Incremental matching permits split packets and discards everything except
/// the status tagged with the unpredictable request token.
pub struct Reply {
    prefix: Vec<u8>,
    matched: usize,
    status: u32,
    digits: u8,
}

impl Reply {
    pub fn new(token: &str) -> Self {
        Self {
            prefix: format!("\x1e{token}:").into_bytes(),
            matched: 0,
            status: 0,
            digits: 0,
        }
    }

    pub fn observe(&mut self, bytes: &[u8]) -> io::Result<Option<u32>> {
        for &byte in bytes {
            if self.matched < self.prefix.len() {
                if byte == self.prefix[self.matched] {
                    self.matched += 1;
                } else {
                    self.matched = usize::from(byte == self.prefix[0]);
                }
            } else if byte == b'\n' && self.digits != 0 {
                return Ok(Some(self.status));
            } else if byte.is_ascii_digit() && self.digits < 3 {
                self.digits += 1;
                self.status = self.status * 10 + u32::from(byte - b'0');
                if self.status > 255 {
                    return Err(invalid_reply());
                }
            } else {
                return Err(invalid_reply());
            }
        }
        Ok(None)
    }
}

fn invalid_reply() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Invalid SSH sudo terminal reply",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sudo_terminal_reply_requires_the_current_token_across_packet_boundaries() {
        let mut reply = Reply::new("fixture");
        assert_eq!(reply.observe(b"output\x1eother:0\n\x1efix").unwrap(), None);
        assert_eq!(reply.observe(b"ture:2").unwrap(), None);
        assert_eq!(reply.observe(b"55\n").unwrap(), Some(255));
        assert!(Reply::new("fixture").observe(b"\x1efixture:999\n").is_err());
    }

    #[test]
    fn sudo_terminal_validates_before_accepting_commands() {
        let script = command([0; 16]);
        assert!(script.find("-S -p").unwrap() < script.find("while IFS=").unwrap());
        assert!(script.contains("-v -u root\n"));
        assert!(script.contains("</dev/null"));
        assert!(script.contains("stty raw -echo"));
    }
}
