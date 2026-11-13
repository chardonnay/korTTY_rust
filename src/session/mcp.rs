use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use async_channel::{Receiver, Sender, TrySendError, bounded};
use kortty_control::{Fault, Reply, Screen};
use kortty_terminal::Terminal;
use serde_json::Value;
use zeroize::Zeroizing;

use super::{
    Backend, Event, GuardedInput, InputError, Session, Shared, Status, StreamInput,
    StreamTransport, WriteReply, ai_code_guard, send_input,
};

pub(super) type ReadFilter =
    Box<dyn FnOnce(Screen, Vec<bool>, bool) -> Result<Value, Fault> + Send>;

pub(crate) enum McpIdentity {
    Local,
    Ssh { user: String, host: String },
}

#[derive(Clone)]
pub(crate) struct McpGuard(Arc<Permit>);

struct Permit {
    reply: Reply,
    identity: McpIdentity,
    revision: u64,
    input_revision: u64,
    cancelled: AtomicBool,
}

pub(crate) struct McpInput {
    pub(super) bytes: Zeroizing<Vec<u8>>,
    pub(super) guard: McpGuard,
    pub(super) submits: bool,
    pub(super) changes_prompt: bool,
}

impl McpInput {
    pub(crate) fn new(
        bytes: Vec<u8>,
        identity: McpIdentity,
        revision: u64,
        input_revision: u64,
        submits: bool,
        changes_prompt: bool,
        reply: Reply,
    ) -> Self {
        Self {
            bytes: Zeroizing::new(bytes),
            guard: McpGuard(Arc::new(Permit {
                reply,
                identity,
                revision,
                input_revision,
                cancelled: AtomicBool::new(false),
            })),
            submits,
            changes_prompt,
        }
    }

    pub(crate) fn guard(&self) -> McpGuard {
        self.guard.clone()
    }

    pub(crate) fn matches(&self, bytes: &[u8], revision: u64) -> bool {
        self.bytes.as_slice() == bytes && self.guard.0.revision == revision
    }
}

type Preflight = Result<(), &'static str>;
pub(super) type Check = Sender<Preflight>;

impl Session {
    pub(crate) fn mcp_input_revision(&self) -> u64 {
        self.shared.output_activity.input_revision()
    }

    pub(crate) fn mcp_pacing(&self) -> bool {
        self.shared.typewriter_speed().is_some()
    }

    pub(crate) fn mcp_preflight(&self, guard: McpGuard) -> Result<Receiver<Preflight>, InputError> {
        let Backend::Stream { events, actor, .. } = &self.backend else {
            return Err(InputError::Closed);
        };
        let (reply, receive) = bounded(1);
        events
            .try_send(Event::McpPreflight(guard, reply))
            .map_err(|error| match error {
                TrySendError::Full(_) => InputError::Busy,
                TrySendError::Closed(_) => InputError::Closed,
            })?;
        actor.unpark();
        Ok(receive)
    }

    pub(crate) fn send_mcp_input(
        &self,
        input: McpInput,
    ) -> Result<Receiver<Result<usize, &'static str>>, InputError> {
        let Backend::Stream { input: queue, .. } = &self.backend else {
            return Err(InputError::Closed);
        };
        let (reply, receive) = bounded(1);
        let permit = input.guard();
        send_input(
            queue,
            StreamInput::Guarded(GuardedInput::Mcp(input), WriteReply::Mcp(reply, permit)),
        )?;
        Ok(receive)
    }
}

pub(super) struct Live<'a> {
    pub terminal: &'a Terminal,
    pub shared: &'a Shared,
    pub status: &'a Status,
    pub transport: &'a mut StreamTransport,
    pub output: Option<Instant>,
    pub revision: u64,
}

impl McpGuard {
    pub(crate) fn active(&self) -> bool {
        !self.0.cancelled.load(Ordering::Acquire) && self.0.reply.active()
    }

    pub(crate) fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Release);
    }

    pub(super) fn check(&self, live: Live<'_>) -> Preflight {
        if !self.active() {
            return Err("request_cancelled");
        }
        if *live.status != Status::Running || live.shared.serial.is_some() {
            return Err("pane_state_unknown");
        }
        if live.terminal.alternate_screen_active() {
            return Err("alternate_screen");
        }
        if live.terminal.cursor_line_is_password_prompt() {
            return Err("password_prompt");
        }
        if live.shared.typewriter_speed().is_some() {
            return Err("paste_pacing");
        }
        if live.shared.output_activity.last_output() != live.output
            || self.0.revision != live.revision
            || live.shared.output_activity.input_revision() != self.0.input_revision
        {
            return Err("terminal_changed");
        }
        let screen = live.terminal.snapshot();
        if ai_code_guard::coding_agent_visible(&screen)
            || ai_code_guard::approval_prompt_visible(&screen)
        {
            return Err("coding_agent");
        }
        let verified = match (live.transport, &self.0.identity) {
            (StreamTransport::Pty(pty), McpIdentity::Local) => pty.mcp_shell_is_foreground(),
            (StreamTransport::Ssh(ssh), McpIdentity::Ssh { user, host }) => {
                ssh.verified_host_key().is_some()
                    && live.shared.shell_activity.as_ref().is_some_and(|activity| {
                        activity.lock().is_ok_and(|activity| {
                            crate::workspace::mcp_ssh_prompt_is_verified(
                                &activity.snapshot(),
                                &screen,
                                user,
                                host,
                            )
                        })
                    })
            }
            _ => false,
        };
        if !verified {
            return Err("foreign_or_unknown_shell");
        }
        if !self.active() {
            return Err("request_cancelled");
        }
        Ok(())
    }
}
