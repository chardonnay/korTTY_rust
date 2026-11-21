//! Private POSIX PTY for sudo validation and later reviewed commands.

use std::io::{self, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use kortty::ssh_runtime::sudo_pty;
use kortty_pty::{Pty, PtyReader, PtyWriter};
use kortty_ssh::SensitiveLine;
use zeroize::{Zeroize, Zeroizing};

use super::{LocalDirectory, POLL, Status, auth_status, retry, stopped_or_failed};

const CHALLENGE_LIMIT: Duration = Duration::from_secs(30);
const ANSWER_LIMIT: Duration = Duration::from_secs(5 * 60);

pub(super) struct Streams {
    _pty: Pty,
    reader: PtyReader,
    writer: PtyWriter,
}

impl Streams {
    fn spawn(directory: &LocalDirectory, arguments: Vec<String>) -> Result<Self, Status> {
        let file = directory
            .open_verified()
            .map_err(|_| Status::TransportFailed)?;
        let mut pty =
            Pty::spawn_in_directory(arguments, file).map_err(|_| Status::TransportFailed)?;
        let reader = pty.take_reader().ok_or(Status::TransportFailed)?;
        let writer = pty.take_writer().ok_or(Status::TransportFailed)?;
        Ok(Self {
            _pty: pty,
            reader,
            writer,
        })
    }

    pub(super) fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let result = self.reader.read_nonblocking(bytes);
        if matches!(&result, Err(error) if retry(error)) {
            std::thread::sleep(POLL);
        }
        result
    }

    pub(super) fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let result = self.writer.write_nonblocking(bytes);
        if matches!(&result, Err(error) if retry(error)) {
            std::thread::sleep(POLL);
        }
        result
    }
}

pub(super) fn supported() -> bool {
    #[cfg(target_os = "linux")]
    if kortty_pty::flatpak::running() {
        return false;
    }
    Path::new("/usr/bin/sudo").is_file()
}

pub(super) fn open(
    directory: &LocalDirectory,
    nonce: [u8; 16],
    permitted: impl Fn() -> bool,
    answers: async_channel::Receiver<SensitiveLine>,
    challenge: impl FnOnce() -> bool,
) -> Result<Streams, Status> {
    if !permitted() {
        return Err(Status::Cancelled);
    }
    if !supported() {
        return Err(Status::TransportFailed);
    }
    let mut streams = Streams::spawn(directory, sudo_pty::local_arguments(nonce))?;
    authenticate(&mut streams, nonce, &permitted, answers, challenge)?;
    Ok(streams)
}

fn authenticate(
    streams: &mut Streams,
    nonce: [u8; 16],
    permitted: &impl Fn() -> bool,
    answers: async_channel::Receiver<SensitiveLine>,
    challenge: impl FnOnce() -> bool,
) -> Result<(), Status> {
    let mut challenge = Some(challenge);
    let mut submitted = false;
    let marker = kortty::ssh_runtime::password::marker(nonce);
    let mut validation = sudo_pty::Validation::new(nonce, &marker);
    let mut buffer = Zeroizing::new([0; 4096]);
    let mut deadline = Instant::now() + CHALLENGE_LIMIT;
    loop {
        if !permitted() {
            return Err(Status::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(Status::TimedOut);
        }
        match streams.read(&mut buffer[..]) {
            Ok(0) => return Err(Status::MissingExitCode),
            Ok(count) => {
                let event = validation.observe(&buffer[..count]);
                buffer[..count].zeroize();
                let (completed, prompted) = event.map_err(auth_status)?;
                if let Some(code) = completed {
                    return match code {
                        0 if challenge.is_some() || submitted => Ok(()),
                        0 => Err(Status::TransportFailed),
                        _ => Err(Status::Exited(code)),
                    };
                }
                if prompted {
                    let notify = challenge.take().ok_or(Status::Exited(1))?;
                    if !notify() {
                        return Err(Status::Cancelled);
                    }
                    deadline = Instant::now() + ANSWER_LIMIT;
                }
            }
            Err(error) if retry(&error) => {}
            Err(_) => return Err(stopped_or_failed(permitted)),
        }
        if !submitted {
            match answers.try_recv() {
                Ok(secret) if challenge.is_none() => {
                    let mut input = Input {
                        streams,
                        permitted,
                        deadline,
                    };
                    secret.write_unframed(&mut input).map_err(|_| {
                        if Instant::now() >= deadline {
                            Status::TimedOut
                        } else {
                            stopped_or_failed(permitted)
                        }
                    })?;
                    submitted = true;
                }
                Ok(_) | Err(async_channel::TryRecvError::Closed) => return Err(Status::Cancelled),
                Err(async_channel::TryRecvError::Empty) => {}
            }
        }
    }
}

struct Input<'a, F> {
    streams: &'a mut Streams,
    permitted: &'a F,
    deadline: Instant,
}

impl<F: Fn() -> bool> Write for Input<'_, F> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        while (self.permitted)() && Instant::now() < self.deadline {
            match self.streams.write(bytes) {
                Err(error) if retry(&error) => {}
                result => return result,
            }
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Local sudo input expired",
        ))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snippets::tests::Profile;
    use crate::terminal_agent::password::{Endpoint, Terminal};

    #[test]
    fn local_sudo_owns_the_pty_and_keeps_authentication_separate_from_command_input() {
        let profile = Profile::new();
        let directory = LocalDirectory::capture(profile.0.canonicalize().unwrap()).unwrap();
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).unwrap();
        let mut arguments = sudo_pty::local_arguments(nonce);
        arguments[2] = r#"
/bin/stty raw -echo -echonl
printf '%s\n%s' "$3" "$1"
IFS= read -r supplied || exit 1
[ "$supplied" = local-sudo-fixture ] || exit 2
printf '\036%s:0\n' "$2"
IFS=' ' read -r request encoded || exit 3
decoded=$(printf '%b.' "$encoded")
[ "${decoded%.}" = 'id -u' ] || exit 4
printf '%s\n' "$PWD" > approved-command
printf '\036%s:0\n' "$request"
"#
        .into();
        let mut streams = Streams::spawn(&directory, arguments).unwrap();
        let (submit, answers) = async_channel::bounded(1);
        authenticate(&mut streams, nonce, &|| true, answers, || {
            submit
                .try_send(SensitiveLine::new("local-sudo-fixture").unwrap())
                .is_ok()
        })
        .unwrap();
        let receipt = directory.path().join("approved-command");
        assert!(!receipt.exists());
        let mut terminal = Terminal {
            endpoint: Endpoint::Local(directory.clone()),
            streams: super::super::Streams::Local(Box::new(streams)),
        };
        let mut request_nonce = [0; 16];
        getrandom::fill(&mut request_nonce).unwrap();
        assert_eq!(
            terminal.execute(&sudo_pty::token(request_nonce), "id -u", &|| true),
            Status::Exited(0)
        );
        let received = std::fs::read_to_string(receipt).unwrap();
        assert_eq!(received.trim_end(), directory.path().to_str().unwrap());
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires the disposable Fedora sudo/PAM fixture root"]
    fn local_sudo_authenticates_with_real_pam_in_a_disposable_root() {
        use std::os::unix::fs::MetadataExt as _;

        use crate::terminal_agent::Cancel;
        use crate::terminal_agent::password::{AUTHENTICATION_REJECTED, Command};

        assert_eq!(
            std::fs::read_to_string("/etc/kortty-sudo-test-root").unwrap(),
            "Disposable korTTY sudo/PAM fixture\n"
        );
        assert_eq!(rustix::process::geteuid().as_raw(), 1000);
        let directory = LocalDirectory::capture("/work".into()).unwrap();
        let endpoint = Endpoint::Local(directory.clone());
        let validate = |secret| {
            let (submit, answers) = async_channel::bounded(1);
            let mut challenged = false;
            let result = Terminal::authenticate(
                endpoint.clone(),
                || true,
                answers,
                || {
                    challenged = true;
                    submit.try_send(SensitiveLine::new(secret).unwrap()).is_ok()
                },
            );
            assert!(
                challenged,
                "sudo must prompt within the disposable PAM fixture"
            );
            result
        };
        assert!(matches!(
            validate("wrong-fixture-password"),
            Err(AUTHENTICATION_REJECTED)
        ));
        let terminal = validate("correct-fixture-password").unwrap().unwrap();
        let result = directory.path().join("effective-uid");
        assert!(!result.exists());
        let command = Command::new("/work", "/usr/bin/id -u > /work/effective-uid", true).unwrap();
        let (_submit, answers) = async_channel::bounded(1);
        let (status, terminal) = command
            .run_with_terminal(
                endpoint,
                Some(terminal),
                Cancel::default(),
                || true,
                answers,
                || panic!("the approved command must use the authenticated terminal"),
            )
            .unwrap();
        assert_eq!(status, Status::Exited(0));
        assert_eq!(std::fs::read_to_string(&result).unwrap(), "0\n");
        assert_eq!(std::fs::metadata(result).unwrap().uid(), 0);
        drop(terminal);
    }
}
