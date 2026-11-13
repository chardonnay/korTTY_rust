//! Guarded insertion of a reviewed agent command into an empty shell prompt.

#[cfg(windows)]
use std::path::PathBuf;

use async_channel::{Receiver, bounded};
use kortty_terminal::{TerminalProfile, TextEncoding};
use zeroize::Zeroizing;

use super::{
    Backend, GuardedInput, InputError, Session, ShortcutPrompt, Status, StreamInput,
    StreamTransport, WriteReply, ai_code_guard, mcp, send_input,
};
use crate::ssh_connection::ExecOpener;
use crate::terminal_agent::{Cancel, LocalDirectory, valid_target_label};

pub(crate) enum Destination {
    Local(LocalDirectory),
    Ssh {
        opener: ExecOpener,
        user: String,
        host: String,
        directory: String,
    },
}

pub(crate) struct Request {
    pub destination: Destination,
    pub prompt: ShortcutPrompt,
    pub screen_revision: u64,
    pub input_revision: u64,
    pub permit: Cancel,
    command: Zeroizing<String>,
}

impl Request {
    pub(crate) fn new(
        destination: Destination,
        prompt: ShortcutPrompt,
        screen_revision: u64,
        input_revision: u64,
        permit: Cancel,
        command: String,
    ) -> Result<Self, &'static str> {
        if !valid_target_label(&command) || command.trim().is_empty() {
            return Err("Only an exact, single-line command can be taken over.");
        }
        Ok(Self {
            destination,
            prompt,
            screen_revision,
            input_revision,
            permit,
            command: Zeroizing::new(command),
        })
    }

    pub(super) fn authorize(self, live: mcp::Live<'_>) -> Result<Zeroizing<Vec<u8>>, &'static str> {
        const CHANGED: &str =
            "The prompt or terminal changed before the command could be inserted.";
        if self.permit.requested()
            || *live.status != Status::Running
            || live.terminal.profile() != TerminalProfile::Xterm
            || !live.terminal.bracketed_paste()
            || live.terminal.alternate_screen_active()
            || live.terminal.cursor_line_is_password_prompt()
            || live.shared.typewriter_speed().is_some()
            || live.shared.output_activity.last_output() != live.output
            || live.shared.output_activity.input_revision() != self.input_revision
            || live.revision != self.screen_revision
        {
            return Err(CHANGED);
        }
        let mut activity = live
            .shared
            .shell_activity
            .as_ref()
            .ok_or(CHANGED)?
            .lock()
            .map_err(|_| CHANGED)?;
        if activity.agent_shortcut_prompt() != Some(self.prompt) {
            return Err(CHANGED);
        }
        let screen = live.terminal.snapshot();
        if screen.text_encoding() != TextEncoding::Utf8
            || ai_code_guard::coding_agent_visible(&screen)
            || ai_code_guard::approval_prompt_visible(&screen)
        {
            return Err(CHANGED);
        }
        let shell = activity.snapshot();
        let report = shell
            .directory
            .as_ref()
            .filter(|report| report.sequence.saturating_add(2) >= shell.prompt);
        let destination = match (self.destination, live.transport) {
            (Destination::Local(expected), StreamTransport::Pty(pty)) => {
                #[cfg(unix)]
                let path = pty.shell_directory().ok();
                #[cfg(windows)]
                let path = report
                    .filter(|report| report.host.is_empty() || report.host.as_ref() == "localhost")
                    .and_then(|report| report.path.strip_prefix('/'))
                    .filter(|_| pty.mcp_shell_is_foreground())
                    .map(PathBuf::from);
                path.and_then(|path| LocalDirectory::capture(path).ok())
                    .is_some_and(|current| {
                        current.path() == expected.path() && expected.require_current().is_ok()
                    })
            }
            (
                Destination::Ssh {
                    opener,
                    user,
                    host,
                    directory,
                },
                StreamTransport::Ssh(ssh),
            ) => {
                ssh.verified_host_key().is_some()
                    && ssh.verified_host_key() == opener.verified_host_key()
                    && ssh
                        .exec_opener()
                        .is_some_and(|current| current.same_connection(&opener))
                    && crate::workspace::mcp_ssh_prompt_is_verified(&shell, &screen, &user, &host)
                    && report.is_some_and(|report| report.path.as_ref() == directory)
            }
            _ => false,
        };
        if !destination || self.permit.requested() {
            return Err(
                "The shell's foreground process, trust or working directory could not be verified.",
            );
        }
        let mut bytes = Zeroizing::new(Vec::with_capacity(self.command.len() + 12));
        bytes.extend_from_slice(b"\x1b[200~");
        bytes.extend_from_slice(self.command.as_bytes());
        bytes.extend_from_slice(b"\x1b[201~");
        activity.untracked_input();
        Ok(bytes)
    }
}

impl Session {
    pub(crate) fn take_over_agent_command(
        &self,
        request: Request,
    ) -> Result<Receiver<Result<usize, &'static str>>, InputError> {
        let Backend::Stream { input, .. } = &self.backend else {
            return Err(InputError::Closed);
        };
        let (reply, receive) = bounded(1);
        let permit = request.permit.clone();
        send_input(
            input,
            StreamInput::Guarded(
                GuardedInput::AgentTakeover(request),
                WriteReply::AgentTakeover(reply, permit),
            ),
        )?;
        Ok(receive)
    }
}
