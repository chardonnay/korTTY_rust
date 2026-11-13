use std::sync::atomic::Ordering;
use std::time::Instant;

use kortty_terminal::{ScreenSnapshot, Terminal};
use zeroize::Zeroizing;

use crate::input::{encode_stream_paste, sanitize_paste};
use crate::session::{MAX_INPUT_BYTES, PromptState, Shared, Status};

use super::{CodingAgentState, CodingAgentStatus, Monitor, approval_prompt_visible};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CodingAgentKey {
    Yes,
    No,
    Enter,
    Escape,
    Up,
    Down,
    Interrupt,
}

impl CodingAgentKey {
    fn bytes(self) -> &'static [u8] {
        match self {
            Self::Yes => b"YES\r",
            Self::No => b"NO\r",
            Self::Enter => b"\r",
            Self::Escape => b"\x1b",
            Self::Up => b"\x1bOA",
            Self::Down => b"\x1bOB",
            Self::Interrupt => b"\x03",
        }
    }

    fn decision(self) -> bool {
        matches!(self, Self::Yes | Self::No | Self::Enter | Self::Escape)
    }

    fn submits(self) -> bool {
        matches!(self, Self::Yes | Self::No | Self::Enter)
    }
}

pub(crate) enum CodingAgentInput {
    Key(CodingAgentKey),
    Keys {
        bytes: Zeroizing<Vec<u8>>,
        decision: bool,
        submits: bool,
    },
    Prompt(Zeroizing<String>),
}

impl CodingAgentInput {
    pub(in crate::session) fn exceeds_limit(&self) -> bool {
        match self {
            Self::Prompt(text) => text.len() > MAX_INPUT_BYTES,
            Self::Keys { bytes, .. } => bytes.len() > MAX_INPUT_BYTES,
            Self::Key(_) => false,
        }
    }
}

pub(in crate::session) struct InputRequest {
    pub(in crate::session) expected: CodingAgentStatus,
    pub(in crate::session) expected_revision: u64,
    pub(in crate::session) input: CodingAgentInput,
}

const CHANGED: &str = "The agent or terminal screen changed. Choose the action again.";
const BLOCKED: &str = "The agent is waiting for a decision. Answer it before sending a prompt.";

pub(in crate::session) fn authorize_agent_input(
    terminal: &Terminal,
    shared: &Shared,
    status: &Status,
    monitor: Option<&mut Monitor>,
    applied_output: Option<Instant>,
    revision: u64,
    request: InputRequest,
) -> Result<Zeroizing<Vec<u8>>, &'static str> {
    let Some(monitor) = monitor.filter(|_| *status == Status::Running) else {
        return Err("The coding agent's terminal is disconnected.");
    };
    if shared.typewriter_speed.load(Ordering::Relaxed) != 0 {
        return Err("The terminal pane is busy.");
    }
    let decision = (matches!(&request.input, CodingAgentInput::Key(key) if key.decision())
        || matches!(
            &request.input,
            CodingAgentInput::Keys { decision: true, .. }
        ))
        && request.expected.state == CodingAgentState::Blocked;
    if shared.output_activity.last_output() != applied_output
        || decision && request.expected_revision != revision
    {
        return Err(CHANGED);
    }
    let screen = terminal.snapshot();
    let preference = shared.coding_agent_detection.load(Ordering::Acquire);
    let live_state = monitor
        .input_state(preference, request.expected, &screen)
        .ok_or(CHANGED)?;
    let bytes = match request.input {
        CodingAgentInput::Keys { bytes, submits, .. } => {
            if submits && terminal.cursor_line_is_password_prompt() {
                return Err("The terminal is waiting for a password.");
            }
            bytes
        }
        CodingAgentInput::Key(key) => {
            if key.submits() && terminal.cursor_line_is_password_prompt() {
                return Err("The terminal is waiting for a password.");
            }
            Zeroizing::new(key.bytes().to_vec())
        }
        CodingAgentInput::Prompt(text) => {
            if live_state == CodingAgentState::Blocked || approval_prompt_visible(&screen) {
                return Err(BLOCKED);
            }
            if terminal.cursor_line_is_password_prompt() {
                return Err("The terminal is waiting for a password.");
            }
            prompt_bytes(&screen, &text, terminal.bracketed_paste())?
        }
    };
    let mut shell = shared
        .shell_activity
        .as_ref()
        .map(|state| state.lock().unwrap_or_else(|error| error.into_inner()));
    if shared.output_activity.last_output() != applied_output
        || shared.coding_agent_detection.load(Ordering::Acquire) != preference
        || shell
            .as_ref()
            .is_some_and(|state| state.code_prompt().state == PromptState::Ready)
    {
        return Err(CHANGED);
    }
    if decision && !monitor.reserve_decision(request.expected.instance, revision) {
        return Err("This decision was already sent. Wait for the agent to respond.");
    }
    if let Some(state) = &mut shell {
        state.untracked_input();
    }
    Ok(bytes)
}

fn prompt_bytes(
    screen: &ScreenSnapshot,
    text: &str,
    bracketed_paste: bool,
) -> Result<Zeroizing<Vec<u8>>, &'static str> {
    let normalized = Zeroizing::new(sanitize_paste(text));
    let normalized = normalized.trim_end_matches('\n');
    if normalized.trim().is_empty() {
        return Err("Enter a prompt before sending.");
    }
    let bracketed = normalized.contains('\n');
    if bracketed && !bracketed_paste {
        return Err("Multiline prompts require bracketed paste in the agent's terminal.");
    }
    let with_cr = Zeroizing::new(normalized.replace('\n', "\r"));
    let mut bytes = Zeroizing::new(encode_stream_paste(screen, &with_cr, bracketed));
    if bytes.iter().filter(|&&byte| byte == b'?').count()
        != normalized.bytes().filter(|&byte| byte == b'?').count()
    {
        return Err("The terminal encoding cannot represent this prompt exactly.");
    }
    bytes.push(b'\r');
    if bytes.len() > MAX_INPUT_BYTES {
        return Err("The prompt exceeds the terminal input limit.");
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kortty_terminal::TerminalProfile;

    #[test]
    fn coding_agent_input_matches_the_java_key_and_prompt_bytes() {
        let terminal = Terminal::with_profile(80, 24, TerminalProfile::Xterm);
        let screen = terminal.snapshot();
        assert_eq!(CodingAgentKey::Yes.bytes(), b"YES\r");
        assert_eq!(CodingAgentKey::No.bytes(), b"NO\r");
        assert_eq!(CodingAgentKey::Enter.bytes(), b"\r");
        assert_eq!(CodingAgentKey::Escape.bytes(), b"\x1b");
        assert_eq!(CodingAgentKey::Up.bytes(), b"\x1bOA");
        assert_eq!(CodingAgentKey::Down.bytes(), b"\x1bOB");
        assert_eq!(CodingAgentKey::Interrupt.bytes(), b"\x03");
        assert_eq!(
            prompt_bytes(&screen, "hello\r\n", true).unwrap().as_slice(),
            b"hello\r"
        );
        assert_eq!(
            prompt_bytes(&screen, "hello", false).unwrap().as_slice(),
            b"hello\r"
        );
        let text = "first\r\n\x1b[201~second\rthird\u{9b}200~\n";
        assert_eq!(
            prompt_bytes(&screen, text, true).unwrap().as_slice(),
            b"\x1b[200~first\rsecond\rthird\x1b[201~\r"
        );
        assert!(matches!(
            prompt_bytes(&screen, text, false),
            Err("Multiline prompts require bracketed paste in the agent's terminal.")
        ));
        assert!(prompt_bytes(&screen, "\x1b[201~\n\t", true).is_err());
    }

    #[test]
    fn api_key_sequences_share_the_actor_input_bound() {
        let sequence = |size| CodingAgentInput::Keys {
            bytes: Zeroizing::new(vec![b'y'; size]),
            decision: true,
            submits: false,
        };
        assert!(!sequence(MAX_INPUT_BYTES).exceeds_limit());
        assert!(sequence(MAX_INPUT_BYTES + 1).exceeds_limit());
    }
}
