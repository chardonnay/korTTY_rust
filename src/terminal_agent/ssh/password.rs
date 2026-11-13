//! A single password submission to a reviewed, authenticated SSH exec lease.

use std::time::{Duration, Instant};

use kortty::ssh_runtime::{mux::PasswordEvent, password};
use kortty_ssh::SensitiveLine;

use crate::{ssh_connection::ExecOpener, ssh_worker};

use super::super::runner::{COMMAND_LIMIT, Cancel, POLL, Status};
use super::{quote, validate_remote_command};

pub(crate) struct Command {
    wire: String,
    nonce: [u8; 16],
}

impl Command {
    pub(crate) fn new(directory: &str, command: &str) -> Result<Self, String> {
        validate_remote_command(directory, command)?;
        let mut random = [0; 16];
        getrandom::fill(&mut random).map_err(|_| "Could not create a sudo challenge.")?;
        let marker = password::marker(random);
        let payload = format!(
            "exec </dev/null\ncd {} || exit\n{command}",
            quote(directory)
        );
        let wire = format!(
            "exec /usr/bin/sudo -k -S -p {} -u root -- /bin/sh -c {}",
            quote(&marker),
            quote(&payload)
        );
        if wire.len() > kortty_ssh::MAX_EXEC_COMMAND_BYTES {
            return Err("The effective sudo command exceeds the SSH execution limit.".into());
        }
        Ok(Self {
            wire,
            nonce: random,
        })
    }

    pub(crate) fn wire(&self) -> &str {
        &self.wire
    }

    pub(crate) fn run(
        self,
        opener: ExecOpener,
        cancel: Cancel,
        authorized: impl Fn() -> bool + Clone + Send + Sync + 'static,
        answers: async_channel::Receiver<SensitiveLine>,
        challenge: impl FnOnce() -> bool,
    ) -> Result<Status, &'static str> {
        let permitted = move || !cancel.requested() && authorized();
        let status = match opener {
            ExecOpener::Direct(opener) => {
                let opening = permitted.clone();
                let lease = opener
                    .open_sensitive_cancellable(&self.wire, move || !opening())
                    .map_err(|_| "Could not open the reviewed sudo SSH channel.")?;
                password::supervise(
                    lease,
                    &password::marker(self.nonce),
                    permitted,
                    || match answers.try_recv() {
                        Ok(line) => password::Answer::Line(line),
                        Err(async_channel::TryRecvError::Closed) => password::Answer::Closed,
                        Err(async_channel::TryRecvError::Empty) => password::Answer::Pending,
                    },
                    challenge,
                )?
            }
            ExecOpener::Worker(opener) => self.run_worker(opener, permitted, answers, challenge)?,
        };
        Ok(match status {
            password::Status::Exited(code) => Status::Exited(code),
            password::Status::Cancelled => Status::Cancelled,
            password::Status::TimedOut => Status::TimedOut,
            password::Status::TransportFailed => Status::TransportFailed,
            password::Status::MissingExitCode => Status::MissingExitCode,
        })
    }

    fn run_worker(
        self,
        opener: ssh_worker::Opener,
        permitted: impl Fn() -> bool + Clone + Send + Sync + 'static,
        answers: async_channel::Receiver<SensitiveLine>,
        challenge: impl FnOnce() -> bool,
    ) -> Result<password::Status, &'static str> {
        let channel = opener
            .open_password_exec(&self.wire, self.nonce, || !permitted())
            .map_err(|_| "Could not open the reviewed isolated SSH sudo channel.")?;
        let deadline = Instant::now() + COMMAND_LIMIT + Duration::from_secs(10);
        let mut challenge = Some(challenge);
        let mut submitted = false;
        loop {
            if !permitted() {
                return Ok(password::Status::Cancelled);
            }
            if Instant::now() >= deadline {
                return Ok(password::Status::TimedOut);
            }
            match channel
                .next(POLL)
                .map_err(|_| "The isolated SSH sudo channel closed.")?
            {
                Some(PasswordEvent::Challenge) => {
                    if !challenge.take().is_some_and(|notify| notify()) {
                        return Ok(password::Status::Cancelled);
                    }
                }
                Some(PasswordEvent::Staged) => {
                    channel
                        .commit(permitted.clone())
                        .map_err(|_| "The isolated SSH sudo approval expired.")?;
                }
                Some(PasswordEvent::Finished(status)) => return Ok(status),
                None => {}
            }
            if !submitted {
                match answers.try_recv() {
                    Ok(line) if challenge.is_none() && permitted() => {
                        channel
                            .stage(line, permitted.clone())
                            .map_err(|_| "The isolated SSH sudo approval expired.")?;
                        submitted = true;
                    }
                    Ok(_) => {
                        return Err("The isolated SSH sudo password request is no longer valid.");
                    }
                    Err(async_channel::TryRecvError::Closed) => {
                        return Ok(password::Status::Cancelled);
                    }
                    Err(async_channel::TryRecvError::Empty) => {}
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_sudo_wrapper_binds_unique_prompt_and_quoted_working_directory() {
        let first = Command::new("/tmp/it's here", "printf '%s' '$literal'").unwrap();
        let second = Command::new("/tmp/it's here", "printf '%s' '$literal'").unwrap();
        assert_ne!(first.nonce, second.nonce);
        assert!(first.wire.starts_with("exec /usr/bin/sudo -k -S -p '"));
        assert!(
            first
                .wire
                .contains(" -u root -- /bin/sh -c 'exec </dev/null\ncd ")
        );
        assert!(first.wire.contains(&password::marker(first.nonce)));
        assert!(Command::new("relative", "id").is_err());
        assert!(Command::new("/tmp", "echo ***").is_err());
    }

    #[test]
    #[ignore = "requires explicit KORTTY_SSH_EXEC_* host fixtures"]
    fn live_password_sudo_lane_sends_one_fixture_line_and_preserves_shell() {
        use kortty_ssh::{
            Authentication, Connect, Endpoint, HostKey, SshSession, TrustStore, WindowSize,
        };

        let fixture = |suffix| std::env::var(format!("KORTTY_SSH_EXEC_{suffix}")).unwrap();
        let address: std::net::SocketAddr = fixture("ADDRESS").parse().unwrap();
        let endpoint = Endpoint::new(&address.ip().to_string(), address.port()).unwrap();
        let key = HostKey::from_openssh(&fixture("HOST_KEY")).unwrap();
        let trust = TrustStore::from_pins(vec![(endpoint.clone(), key)]).unwrap();
        let mut shell = SshSession::connect(
            Connect {
                address,
                endpoint,
                username: fixture("USER"),
                authentication: Authentication::private_key(
                    std::fs::read(fixture("PRIVATE_KEY")).unwrap(),
                ),
                term: "xterm".into(),
                size: WindowSize {
                    columns: 80,
                    rows: 24,
                    pixel_width: 0,
                    pixel_height: 0,
                },
            },
            trust,
        )
        .unwrap();
        let opener = shell.exec_opener().unwrap();
        let nonce = [21; 16];
        let marker = password::marker(nonce);
        // A finite fake challenge exercises the actual lease without invoking sudo.
        let script = format!(
            "printf '%s' {} >&2; IFS= read -r value || exit 70; test \"${{#value}}\" -eq 14 || exit 71; IFS= read -r extra && exit 72; printf '%s' \"$value\"; exit 29",
            quote(&marker),
        );
        let command = Command {
            wire: format!("exec /usr/bin/timeout 8 /bin/sh -c {}", quote(&script)),
            nonce,
        };
        let (sender, answers) = async_channel::bounded(1);
        let status = command
            .run(
                ExecOpener::Direct(opener.clone()),
                Cancel::default(),
                || true,
                answers,
                move || {
                    sender
                        .try_send(SensitiveLine::new("synthetic-only").unwrap())
                        .is_ok()
                },
            )
            .unwrap();
        assert_eq!(status, Status::Exited(29));
        assert!(!opener.is_closed());
        shell.close().unwrap();
    }
}
