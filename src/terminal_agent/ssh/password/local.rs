//! Private POSIX PTY for sudo validation and later reviewed commands.

use std::io::{self, Write};
use std::time::{Duration, Instant};

use kortty::ssh_runtime::sudo_pty;
use kortty_pty::{Pty, PtyReader, PtyWriter};
use kortty_ssh::SensitiveLine;
use zeroize::{Zeroize, Zeroizing};

use super::{LocalDirectory, LocalRoute, POLL, Status, auth_status, retry, stopped_or_failed};

#[cfg(target_os = "linux")]
mod flatpak;

const CHALLENGE_LIMIT: Duration = Duration::from_secs(30);
const ANSWER_LIMIT: Duration = Duration::from_secs(5 * 60);

pub(super) struct Streams {
    _pty: Pty,
    reader: PtyReader,
    writer: PtyWriter,
    #[cfg(target_os = "linux")]
    _host_directory: Option<std::fs::File>,
}

impl Streams {
    fn spawn(
        directory: &LocalDirectory,
        route: LocalRoute,
        arguments: Vec<String>,
    ) -> Result<Self, Status> {
        let file = directory
            .open_verified()
            .map_err(|_| Status::TransportFailed)?;
        #[cfg(target_os = "linux")]
        let mut host_directory = None;
        let mut pty = match route {
            LocalRoute::Native => Pty::spawn_in_directory(arguments, file),
            #[cfg(target_os = "linux")]
            LocalRoute::FlatpakHost => {
                let result = flatpak::spawn(directory.path(), arguments, &file);
                host_directory = Some(file);
                result
            }
        }
        .map_err(|_| Status::TransportFailed)?;
        let reader = pty.take_reader().ok_or(Status::TransportFailed)?;
        let writer = pty.take_writer().ok_or(Status::TransportFailed)?;
        Ok(Self {
            _pty: pty,
            reader,
            writer,
            #[cfg(target_os = "linux")]
            _host_directory: host_directory,
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

pub(super) fn open(
    directory: &LocalDirectory,
    route: LocalRoute,
    nonce: [u8; 16],
    permitted: impl Fn() -> bool,
    answers: async_channel::Receiver<SensitiveLine>,
    challenge: impl FnOnce() -> bool,
) -> Result<Streams, Status> {
    if !permitted() {
        return Err(Status::Cancelled);
    }
    if !route.available() {
        return Err(Status::TransportFailed);
    }
    let mut streams = Streams::spawn(directory, route, sudo_pty::local_arguments(nonce))?;
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
        check_private_sudo_pty(&profile.0, LocalRoute::Native);
    }

    fn check_private_sudo_pty(path: &std::path::Path, route: LocalRoute) {
        let directory = LocalDirectory::capture(path.canonicalize().unwrap()).unwrap();
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
        let mut streams = Streams::spawn(&directory, route, arguments).unwrap();
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
            endpoint: Endpoint::Local(directory.clone(), route),
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
    #[ignore = "requires a Flatpak installation and a disposable host-shared test directory"]
    fn flatpak_host_sudo_uses_a_private_pty_and_cancels_a_pending_challenge() {
        use std::path::{Path, PathBuf};

        assert!(LocalRoute::FlatpakHost.available());
        assert!(!LocalRoute::Native.available());
        let path = PathBuf::from(std::env::var_os("KORTTY_FLATPAK_HOST_TEST_ROOT").unwrap());
        assert!(path.is_absolute() && path.join("private-flatpak-test").is_file());
        let workspace = path.join("sudo workspace with 'quotes'");
        std::fs::create_dir(&workspace).unwrap();
        check_private_sudo_pty(&workspace, LocalRoute::FlatpakHost);

        let directory = LocalDirectory::capture(workspace.clone()).unwrap();
        let host = Endpoint::Local(directory.clone(), LocalRoute::FlatpakHost);
        assert!(!host.matches(&Endpoint::Local(directory.clone(), LocalRoute::Native)));

        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).unwrap();
        let mut arguments = sudo_pty::local_arguments(nonce);
        arguments[2] = r#"
[ ! -e /.flatpak-info ] && [ -t 0 ] && [ -t 1 ] && (: <>/dev/tty) || exit 1
/bin/stty raw -echo -echonl || exit 2
printf '%s' "$$" > waiting-sudo-fixture.pid
printf '%s\n%s' "$3" "$1"
IFS= read -r answer || exit 3
printf '%s' "$answer" > cancelled-command
"#
        .into();
        let runtime = LocalDirectory::capture("/usr".into()).unwrap();
        let mut refused =
            Streams::spawn(&runtime, LocalRoute::FlatpakHost, arguments.clone()).unwrap();
        let (_submit, answers) = async_channel::bounded(1);
        assert!(
            authenticate(&mut refused, nonce, &|| true, answers, || {
                panic!("a different host directory must never request a password")
            })
            .is_err()
        );
        drop(refused);

        let mut streams = Streams::spawn(&directory, LocalRoute::FlatpakHost, arguments).unwrap();
        let (_submit, answers) = async_channel::bounded(1);
        let mut prompted = false;
        assert_eq!(
            authenticate(&mut streams, nonce, &|| true, answers, || {
                prompted = true;
                false
            }),
            Err(Status::Cancelled)
        );
        assert!(prompted);
        drop(streams);
        let pid = std::fs::read_to_string(workspace.join("waiting-sudo-fixture.pid")).unwrap();
        assert!(pid.parse::<u32>().is_ok_and(|pid| pid > 1));
        let mut check =
            kortty_pty::flatpak::host_command(Path::new("/usr/bin/test"), &workspace).unwrap();
        assert!(
            check
                .args(["!", "-e", &format!("/proc/{pid}")])
                .status()
                .unwrap()
                .success(),
            "the cancelled host PTY process is still running"
        );
        assert!(!workspace.join("cancelled-command").exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires the disposable Fedora sudo/PAM host or Flatpak fixture"]
    fn local_sudo_authenticates_with_real_pam_in_a_disposable_root() {
        use std::os::unix::fs::MetadataExt as _;
        use std::path::{Path, PathBuf};

        use crate::terminal_agent::Cancel;
        use crate::terminal_agent::password::{AUTHENTICATION_REJECTED, Command};

        let (path, marker, route) = if kortty_pty::flatpak::running() {
            let path =
                PathBuf::from(std::env::var_os("KORTTY_FLATPAK_PAM_TEST_WORKSPACE").unwrap());
            let output = kortty_pty::flatpak::host_command(Path::new("/usr/bin/hostname"), &path)
                .unwrap()
                .output()
                .unwrap();
            assert!(output.status.success());
            assert_eq!(output.stdout, b"kortty-flatpak-fixture\n");
            let output = kortty_pty::flatpak::host_command(Path::new("/usr/bin/id"), &path)
                .unwrap()
                .arg("-un")
                .output()
                .unwrap();
            assert!(output.status.success());
            assert_eq!(output.stdout, b"fixture\n");
            let marker = path.join("kortty-sudo-test-root");
            (path, marker, LocalRoute::FlatpakHost)
        } else {
            (
                PathBuf::from("/work"),
                PathBuf::from("/etc/kortty-sudo-test-root"),
                LocalRoute::Native,
            )
        };
        assert_eq!(
            std::fs::read_to_string(marker).unwrap(),
            "Disposable korTTY sudo/PAM fixture\n"
        );
        assert_eq!(rustix::process::geteuid().as_raw(), 1000);
        let directory = LocalDirectory::capture(path).unwrap();
        let endpoint = Endpoint::Local(directory.clone(), route);
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
        let command = Command::new(
            directory.path().to_str().unwrap(),
            "/usr/bin/id -u > effective-uid",
            true,
        )
        .unwrap();
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
        drop(terminal);
        if route == LocalRoute::FlatpakHost {
            let output =
                kortty_pty::flatpak::host_command(Path::new("/usr/bin/stat"), directory.path())
                    .unwrap()
                    .args(["-c", "%u", "effective-uid"])
                    .output()
                    .unwrap();
            assert!(output.status.success());
            assert_eq!(output.stdout, b"0\n");
        } else {
            assert_eq!(std::fs::metadata(result).unwrap().uid(), 0);
        }
    }
}
