//! Bounded sudo authentication for an already reviewed SSH exec route.

use std::borrow::Cow;
use std::io::{self, Read};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::Duration;

use kortty_jobs::Cancellation;
use kortty_ssh::SensitiveLine;

use crate::scheduler_window::{SudoMode, script::shell_quote};

use super::Failure;

const AUTH_TIMEOUT: Duration = Duration::from_secs(30);
const EXEC_LIMIT: usize = 16 * 1024;

pub(super) struct Invocation<'a> {
    pub(super) command: Cow<'a, str>,
    pub(super) password: Option<Password<'a>>,
}

pub(super) fn prepare<'a>(
    command: &'a str,
    mode: SudoMode,
    secret: Option<&'a str>,
) -> Result<Invocation<'a>, Failure> {
    if mode == SudoMode::Disabled {
        return Ok(Invocation {
            command: Cow::Borrowed(command),
            password: None,
        });
    }
    let mut wire = String::from("exec /usr/bin/sudo -k ");
    let password = if mode == SudoMode::Vault {
        let secret = secret
            .filter(|secret| kortty_vault::valid_sudo_password(secret))
            .ok_or_else(|| Failure::blocked("A matching vault sudo password is required."))?;
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce)
            .map_err(|_| Failure::failed("Could not create a sudo authentication challenge"))?;
        let marker = format!("[kortty-sudo-{:032x}] ", u128::from_ne_bytes(nonce));
        wire.push_str("-S -p ");
        shell_quote(&mut wire, &marker);
        wire.push(' ');
        Some(Password { marker, secret })
    } else {
        wire.push_str("-n ");
        None
    };
    wire.push_str("-- /bin/sh -c ");
    // The privileged shell never inherits the stream used to authenticate sudo.
    shell_quote(&mut wire, &format!("exec </dev/null\n{command}"));
    if wire.len() > EXEC_LIMIT {
        return Err(Failure::blocked(
            "The sudo wrapper and command exceed the 16 KiB SSH exec limit.",
        ));
    }
    Ok(Invocation {
        command: Cow::Owned(wire),
        password,
    })
}

pub(super) struct Password<'a> {
    marker: String,
    secret: &'a str,
}

impl<'a> Password<'a> {
    pub(super) fn split(self) -> (Prompt, Input<'a>) {
        let (seen, response) = mpsc::sync_channel(1);
        (
            Prompt {
                marker: self.marker,
                matched: 0,
                line_start: true,
                seen: Some(seen),
            },
            Input {
                response,
                secret: self.secret,
            },
        )
    }
}

pub(super) struct Input<'a> {
    response: Receiver<()>,
    secret: &'a str,
}

impl Input<'_> {
    pub(super) fn take_line(
        self,
        cancellation: &Cancellation,
    ) -> io::Result<Option<SensitiveLine>> {
        if self.response.recv_timeout(AUTH_TIMEOUT).is_err() || cancellation.is_requested() {
            return Ok(None);
        }
        SensitiveLine::new(self.secret).map(Some)
    }
}

pub(super) struct Prompt {
    marker: String,
    matched: usize,
    line_start: bool,
    seen: Option<SyncSender<()>>,
}

impl Prompt {
    fn observe(&mut self, bytes: &[u8]) {
        if self.seen.is_none() {
            return;
        }
        let marker = self.marker.as_bytes();
        for byte in bytes {
            if (self.line_start || self.matched > 0) && *byte == marker[self.matched] {
                self.line_start = false;
                self.matched += 1;
                if self.matched == marker.len() {
                    if let Some(seen) = self.seen.take() {
                        let _ = seen.try_send(());
                    }
                    return;
                }
            } else {
                // An echoed command can contain the marker as an argument.
                self.matched = 0;
                self.line_start = *byte == b'\n';
            }
        }
    }
}

pub(super) struct Output<R> {
    reader: R,
    prompt: Option<Prompt>,
}

impl<R> Output<R> {
    pub(super) fn new(reader: R, prompt: Option<Prompt>) -> Self {
        Self { reader, prompt }
    }
}

impl<R: Read> Read for Output<R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = self.reader.read(bytes)?;
        if let Some(prompt) = &mut self.prompt {
            prompt.observe(&bytes[..count]);
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_is_written_once_after_the_exact_fragmented_prompt() {
        let invocation = prepare("printf '%s' 'literal $()'", SudoMode::Vault, Some("sëcret"))
            .unwrap_or_else(|_| panic!("valid invocation"));
        assert!(!invocation.command.contains("sëcret"));
        assert!(
            invocation
                .command
                .starts_with("exec /usr/bin/sudo -k -S -p ")
        );
        assert!(
            invocation
                .command
                .contains("-- /bin/sh -c 'exec </dev/null\n")
        );
        let password = invocation.password.unwrap();
        let marker = password.marker.clone();
        let (mut prompt, input) = password.split();
        prompt.observe(b"Password: [kortty-sudo-wrong] \n");
        prompt.observe(b"+ exec sudo -p '");
        prompt.observe(marker.as_bytes());
        assert!(prompt.seen.is_some());
        prompt.observe(b"'\n");
        for chunk in marker.as_bytes().chunks(3) {
            prompt.observe(chunk);
        }
        assert!(prompt.seen.is_none());
        prompt.observe(marker.as_bytes());
        assert!(input.take_line(&Cancellation::default()).unwrap().is_some());
    }

    #[test]
    fn absent_or_cancelled_prompt_never_writes_the_password() {
        for cancelled in [false, true] {
            let (mut prompt, input) = Password {
                marker: "[kortty-sudo-fixture] ".into(),
                secret: "secret",
            }
            .split();
            let (scheduler, request) = super::super::tests::scheduled_request();
            if cancelled {
                prompt.observe(b"[kortty-sudo-fixture] ");
                scheduler.cancel(request.id).unwrap();
            } else {
                prompt.observe(b"sudo: a password is required");
            }
            drop(prompt);
            assert!(input.take_line(&request.cancellation).unwrap().is_none());
        }
    }

    #[test]
    #[ignore = "requires an explicit pinned KORTTY_SSH_EXEC_* OpenSSH fixture"]
    fn native_sudo_handshake_sends_one_line_over_exec_without_a_pty() {
        use kortty_ssh::{
            Authentication, Connect, Endpoint, HostKey, SshSession, TrustStore, WindowSize,
        };

        let fixture = |name| {
            std::env::var(format!("KORTTY_SSH_EXEC_{name}"))
                .expect("provide the explicit SSH exec fixture")
        };
        let address: std::net::SocketAddr = fixture("ADDRESS").parse().unwrap();
        let endpoint = Endpoint::new(&address.ip().to_string(), address.port()).unwrap();
        let key = HostKey::from_openssh(&fixture("HOST_KEY")).unwrap();
        let trust = TrustStore::from_pins(vec![(endpoint.clone(), key)]).unwrap();
        let invocation = prepare("exit 29", SudoMode::Vault, Some("synthetic-only"))
            .unwrap_or_else(|_| panic!("valid invocation"));
        let password = invocation.password.unwrap();
        // Exercise the real SSH stdin and stderr without elevating the fixture account.
        let mut script = String::from("printf '%s' ");
        shell_quote(&mut script, &password.marker[..12]);
        script.push_str(" >&2; printf '%s' ");
        shell_quote(&mut script, &password.marker[12..]);
        script.push_str(r#" >&2; IFS= read -r value || exit 70; test "${#value}" -eq 14 || exit 71; IFS= read -r extra && exit 72; exit 29"#);
        let mut session = SshSession::connect_exec(
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
            &[],
            &script,
        )
        .unwrap();
        let result =
            super::super::supervise(&mut session, &Cancellation::default(), None, Some(password))
                .unwrap_or_else(|failure| panic!("{}", failure.detail));
        assert_eq!(result, 29);
    }
}
