use std::fmt;
use std::io;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use sunset::packets::UserauthInfoRequest;
use sunset::sshwire::TextString;
use zeroize::Zeroizing;

use super::{STARTUP_CANCEL_POLL, Startup};
use crate::{ConnectError, Endpoint};

const PROMPT_BUDGET: Duration = Duration::from_secs(120);

/// A bounded RFC 4256 authentication round from the verified endpoint.
#[derive(Clone, Debug)]
pub struct KeyboardChallenge {
    pub endpoint: Endpoint,
    pub username: String,
    pub name: String,
    pub instruction: String,
    pub prompts: Vec<KeyboardPrompt>,
    /// Maximum time remaining for this and any following rounds on this endpoint.
    pub response_timeout: Duration,
}

/// The text and input-visibility request for one server prompt.
#[derive(Clone, Debug)]
pub struct KeyboardPrompt {
    pub text: String,
    pub echo: bool,
}

/// One UTF-8 answer. Its owned buffer is cleared on drop.
pub struct KeyboardResponse(Zeroizing<String>);

impl KeyboardResponse {
    pub fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Debug for KeyboardResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("KeyboardResponse([redacted])")
    }
}

type Handler = dyn FnMut(&KeyboardChallenge) -> io::Result<Vec<KeyboardResponse>> + Send;

pub(super) struct InteractiveAuth {
    handler: Option<Box<Handler>>,
    budget: Duration,
}

impl InteractiveAuth {
    pub(super) fn new(
        handler: impl FnMut(&KeyboardChallenge) -> io::Result<Vec<KeyboardResponse>> + Send + 'static,
    ) -> Self {
        Self {
            handler: Some(Box::new(handler)),
            budget: PROMPT_BUDGET,
        }
    }

    pub(super) fn respond(
        &mut self,
        endpoint: &Endpoint,
        username: &str,
        wire: UserauthInfoRequest<'_>,
        startup: &mut Startup<'_>,
    ) -> Result<Vec<KeyboardResponse>, ConnectError> {
        startup.remaining()?;
        if self.budget.is_zero() {
            return Err(timed_out().into());
        }
        let prompts = wire
            .prompts()
            .map(|prompt| {
                Ok(KeyboardPrompt {
                    text: display_text(prompt.text)?,
                    echo: prompt.echo,
                })
            })
            .collect::<Result<Vec<_>, ConnectError>>()?;
        let count = prompts.len();
        let challenge = KeyboardChallenge {
            endpoint: endpoint.clone(),
            username: username.to_owned(),
            name: display_text(wire.name)?,
            instruction: display_text(wire.instruction)?,
            prompts,
            response_timeout: self.budget,
        };
        let began = Instant::now();
        let mut handler = self
            .handler
            .take()
            .ok_or_else(|| io::Error::other("SSH prompt handler missing"))?;
        let (send, receive) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("kortty-ssh-auth-prompt".into())
            .spawn(move || {
                let result = handler(&challenge);
                let _ = send.send((handler, result));
            })?;
        let (handler, result) = loop {
            startup.check_cancelled()?;
            let remaining = self
                .budget
                .checked_sub(began.elapsed())
                .ok_or_else(timed_out)?;
            let wait = if startup.cancelled.is_some() {
                remaining.min(STARTUP_CANCEL_POLL)
            } else {
                remaining
            };
            match receive.recv_timeout(wait) {
                Ok(result) => break result,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::other("SSH prompt handler stopped").into());
                }
            }
        };
        let elapsed = began.elapsed();
        self.budget = self.budget.checked_sub(elapsed).ok_or_else(timed_out)?;
        self.handler = Some(handler);
        startup.deadline += elapsed;
        startup.check_cancelled()?;
        let responses = result?;
        if responses.len() != count {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "SSH keyboard-interactive requires one answer per prompt",
            )
            .into());
        }
        Ok(responses)
    }
}

fn timed_out() -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        "SSH keyboard-interactive response timed out",
    )
}

fn display_text(text: TextString<'_>) -> Result<String, ConnectError> {
    Ok(text
        .to_str()?
        .chars()
        .filter(|&ch| {
            (!ch.is_control() || matches!(ch, '\n' | '\t'))
                && !matches!(ch, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .collect())
}
