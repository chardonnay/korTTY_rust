//! Passive evidence from terminal output and accepted keyboard input.

use std::sync::Arc;
use std::time::{Duration, Instant};

use kortty_terminal::{ShellEvent, ShellEventKind};

const MAX_TYPED_BYTES: usize = 4096;
const TYPED_CD_WINDOW: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum PromptState {
    #[default]
    Unknown,
    Ready,
    Busy,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct CodePrompt {
    pub(crate) state: PromptState,
    revision: u64,
}

/// Identity of the current OSC 133 prompt, independent of unrelated output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ShortcutPrompt(pub(super) u64);

#[derive(Clone, Default)]
pub(crate) struct ShellActivity {
    pub(crate) directory: Option<ReportedDirectory>,
    pub(crate) prompt: u64,
    pub(crate) output: u64,
    pub(crate) typed_cd: Option<TypedCd>,
    pub(crate) command_exit: Option<CommandExit>,
    pub(crate) completed_runtime: Option<Duration>,
    pub(crate) code_prompt: CodePrompt,
}

#[derive(Clone)]
pub(crate) struct ReportedDirectory {
    pub(crate) host: Arc<str>,
    pub(crate) path: Arc<str>,
    pub(crate) sequence: u64,
}

#[derive(Clone)]
pub(crate) struct TypedCd {
    pub(crate) path: Arc<str>,
    pub(crate) sequence: u64,
    pub(crate) prompt_before: u64,
    pub(crate) output_before: u64,
    pub(crate) submitted_at: Instant,
}

#[derive(Clone, Copy)]
pub(crate) struct CommandExit {
    pub(crate) sequence: u64,
    pub(crate) status: Option<i32>,
    pub(crate) started: Option<Instant>,
}

#[derive(Default)]
pub(super) struct State {
    activity: ShellActivity,
    sequence: u64,
    at_prompt: bool,
    saw_command_start: bool,
    ignored_line: bool,
    input: Vec<u8>,
    pending: Option<Pending>,
    command_started: Option<Instant>,
}

struct Pending {
    change: TypedCd,
    phase: Phase,
}

#[derive(PartialEq, Eq)]
enum Phase {
    Submitted,
    Running,
    Succeeded,
}

impl Pending {
    fn fresh_at(&self, at: Instant) -> bool {
        at.checked_duration_since(self.change.submitted_at)
            .is_some_and(|elapsed| elapsed <= TYPED_CD_WINDOW)
    }
}

impl State {
    pub(super) fn snapshot(&self) -> ShellActivity {
        self.activity.clone()
    }

    pub(super) fn code_prompt(&self) -> CodePrompt {
        self.activity.code_prompt
    }

    pub(super) fn agent_shortcut_prompt(&self) -> Option<ShortcutPrompt> {
        (self.at_prompt && self.saw_command_start && self.code_prompt().state == PromptState::Ready)
            .then_some(ShortcutPrompt(self.activity.prompt))
    }

    pub(super) fn journal_prompt(&self) -> Option<u64> {
        (self.at_prompt && self.activity.prompt != 0).then_some(self.activity.prompt)
    }

    fn update_code_prompt(&mut self, state: PromptState) {
        self.activity.code_prompt = CodePrompt {
            state,
            revision: self.next_sequence(),
        };
    }

    pub(super) fn note_code_input(&mut self) {
        let state = match self.code_prompt().state {
            PromptState::Unknown => PromptState::Unknown,
            PromptState::Ready | PromptState::Busy => PromptState::Busy,
        };
        self.update_code_prompt(state);
    }

    fn next_sequence(&mut self) -> u64 {
        self.sequence = self.sequence.saturating_add(1);
        self.sequence
    }

    pub(super) fn output(&mut self, primary: bool, events: Vec<ShellEvent>, received_at: Instant) {
        if primary {
            self.activity.output = self.next_sequence();
            self.activity.code_prompt.revision = self.activity.output;
        }
        for event in events {
            if event.position.alternate_screen {
                continue;
            }
            match event.kind {
                ShellEventKind::WorkingDirectory { host, path } => {
                    self.update_code_prompt(self.code_prompt().state);
                    self.pending = None;
                    self.activity.typed_cd = None;
                    self.activity.directory = Some(ReportedDirectory {
                        host: host.into(),
                        path: path.into(),
                        sequence: self.next_sequence(),
                    });
                }
                ShellEventKind::PromptStart => {
                    self.update_code_prompt(PromptState::Ready);
                    self.activity.prompt = self.next_sequence();
                    self.at_prompt = true;
                    self.saw_command_start = false;
                    self.ignored_line = false;
                    self.input.clear();
                    if let Some(pending) = self.pending.take()
                        && pending.phase == Phase::Succeeded
                        && pending.fresh_at(received_at)
                    {
                        self.activity.typed_cd = Some(pending.change);
                    }
                }
                ShellEventKind::CommandStart => {
                    self.saw_command_start = self.at_prompt;
                    if self.code_prompt().state == PromptState::Unknown {
                        self.update_code_prompt(PromptState::Ready);
                    }
                }
                ShellEventKind::OutputStart => {
                    self.command_started = Some(received_at);
                    self.update_code_prompt(PromptState::Busy);
                    self.at_prompt = false;
                    self.input.clear();
                    if let Some(pending) = self.pending.as_mut()
                        && pending.phase == Phase::Submitted
                        && pending.fresh_at(received_at)
                    {
                        pending.phase = Phase::Running;
                    } else {
                        self.pending = None;
                    }
                }
                ShellEventKind::CommandFinished { exit_status } => {
                    let started = self.command_started.take();
                    self.activity.completed_runtime =
                        started.map(|start| received_at.saturating_duration_since(start));
                    self.update_code_prompt(PromptState::Busy);
                    self.at_prompt = false;
                    self.input.clear();
                    self.activity.command_exit = Some(CommandExit {
                        sequence: self.next_sequence(),
                        status: exit_status,
                        started,
                    });
                    if let Some(pending) = self.pending.as_mut()
                        && pending.phase == Phase::Running
                        && exit_status == Some(0)
                        && pending.fresh_at(received_at)
                    {
                        pending.phase = Phase::Succeeded;
                    } else {
                        self.pending = None;
                    }
                }
            }
        }
        if !primary {
            self.untracked_input();
        }
    }

    pub(super) fn untracked_input(&mut self) {
        self.note_code_input();
        self.at_prompt = false;
        self.input.clear();
        self.pending = None;
        self.activity.typed_cd = None;
    }

    pub(super) fn keyboard(
        &mut self,
        primary: bool,
        bytes: &[u8],
        enter: bool,
        submitted_at: Instant,
    ) {
        if !bytes.is_empty() {
            self.note_code_input();
        }
        if !primary || !self.at_prompt {
            return;
        }
        if enter {
            self.activity.typed_cd = None;
            let path = (matches!(bytes, b"\r" | b"\n" | b"\r\n") && !self.ignored_line)
                .then(|| literal_cd(&self.input).map(Arc::from))
                .flatten();
            self.pending = path.map(|path| Pending {
                change: TypedCd {
                    path,
                    sequence: self.next_sequence(),
                    prompt_before: self.activity.prompt,
                    output_before: self.activity.output,
                    submitted_at,
                },
                phase: Phase::Submitted,
            });
            self.at_prompt = false;
            self.input.clear();
            return;
        }
        if self.ignored_line {
            return;
        }
        for &byte in bytes {
            if matches!(byte, b'\x08' | b'\x7f') {
                self.input.pop();
                continue;
            }
            let possible_prefix = b"cd "
                .get(self.input.len())
                .is_none_or(|&prefix| byte == prefix);
            if !possible_prefix || byte.is_ascii_control() || self.input.len() == MAX_TYPED_BYTES {
                self.input.clear();
                self.ignored_line = true;
                return;
            }
            self.input.push(byte);
        }
    }
}

fn literal_cd(line: &[u8]) -> Option<&str> {
    let arg = std::str::from_utf8(line)
        .ok()?
        .strip_prefix("cd ")?
        .trim_matches(' ');
    let (path, quoted) = match arg.as_bytes().first()? {
        b'\'' => (arg.strip_prefix('\'')?.strip_suffix('\'')?, true),
        b'"' => (arg.strip_prefix('"')?.strip_suffix('"')?, true),
        _ => (arg, false),
    };
    // An explicit path avoids option and home expansion, word splitting, and
    // CDPATH lookup. The remote prompt still has to confirm the command.
    if !(path.starts_with('/') || path.starts_with("./") || path.starts_with("../"))
        || path
            .chars()
            .any(|ch| !(ch.is_alphanumeric() || "._/+-".contains(ch) || quoted && ch == ' '))
    {
        return None;
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kortty_terminal::Terminal;

    fn output(state: &mut State, terminal: &mut Terminal, bytes: &[u8], now: Instant) {
        terminal.advance(bytes);
        state.output(
            !terminal.alternate_screen_active(),
            terminal.take_shell_events(),
            now,
        );
    }

    #[test]
    fn ai_run_prompt_revision_changes_on_input_and_shell_transitions() {
        let mut terminal = Terminal::new(80, 24);
        let mut state = State::default();
        let initial = state.code_prompt();
        assert_eq!(initial.state, PromptState::Unknown);
        output(&mut state, &mut terminal, b"changed output", Instant::now());
        assert_ne!(state.code_prompt(), initial);
        let before_input = state.code_prompt();
        state.note_code_input();
        assert_ne!(state.code_prompt(), before_input);
        assert_eq!(state.code_prompt().state, PromptState::Unknown);
        output(&mut state, &mut terminal, b"\x1b]133;A\x07", Instant::now());
        let prompt = state.code_prompt();
        assert_eq!(prompt.state, PromptState::Ready);
        state.keyboard(true, b"a", false, Instant::now());
        assert_eq!(state.code_prompt().state, PromptState::Busy);
        output(
            &mut state,
            &mut terminal,
            b"\x1b]133;C\x07\x1b]133;D;0\x07",
            Instant::now(),
        );
        assert_eq!(state.code_prompt().state, PromptState::Busy);
        output(&mut state, &mut terminal, b"\x1b]133;A\x07", Instant::now());
        assert_eq!(state.code_prompt().state, PromptState::Ready);
        assert_ne!(state.code_prompt(), prompt);
    }

    #[test]
    fn agent_shortcut_prompt_requires_both_markers_and_no_intervening_input() {
        let now = Instant::now();
        let mut state = State::default();
        let mut terminal = Terminal::new(80, 24);
        output(&mut state, &mut terminal, b"\x1b]133;B\x07", now);
        assert!(state.agent_shortcut_prompt().is_none());
        output(&mut state, &mut terminal, b"\x1b]133;A\x07$ ", now);
        assert!(state.agent_shortcut_prompt().is_none());
        output(&mut state, &mut terminal, b"\x1b]133;B\x07", now);
        let prompt = state.agent_shortcut_prompt().unwrap();
        let revision = state.code_prompt();
        output(&mut state, &mut terminal, b"An asynchronous message", now);
        assert_eq!(state.agent_shortcut_prompt(), Some(prompt));
        assert_ne!(state.code_prompt(), revision);
        state.note_code_input();
        assert!(state.agent_shortcut_prompt().is_none());
        output(&mut state, &mut terminal, b"\x1b]133;B\x07", now);
        assert!(state.agent_shortcut_prompt().is_none());
        output(
            &mut state,
            &mut terminal,
            b"\x1b]133;A\x07$ \x1b]133;B\x07",
            now,
        );
        assert_ne!(state.agent_shortcut_prompt().unwrap(), prompt);
        output(&mut state, &mut terminal, b"\x1b[?1049h", now);
        assert!(state.agent_shortcut_prompt().is_none());
    }

    #[test]
    fn shell_activity_drains_real_osc7_and_osc133_without_alternate_reports() {
        let now = Instant::now();
        let mut state = State::default();
        let mut terminal = Terminal::new(80, 24);
        output(
            &mut state,
            &mut terminal,
            b"\x1b]7;file://EXAMPLE.test/var/nested%",
            now,
        );
        assert!(state.snapshot().directory.is_none());
        output(
            &mut state,
            &mut terminal,
            b"20folder\x1b\\\x1b]133;A\x07",
            now,
        );
        let first = state.snapshot();
        let directory = first.directory.unwrap();
        assert_eq!(&*directory.host, "example.test");
        assert_eq!(&*directory.path, "/var/nested folder");
        assert!(first.prompt > directory.sequence && directory.sequence > first.output);

        output(
            &mut state,
            &mut terminal,
            b"\x1b[?1049h\x1b]7;file://foreign.test/secret\x07\x1b]133;A\x07",
            now,
        );
        let alternate = state.snapshot();
        assert_eq!(alternate.prompt, first.prompt);
        assert_eq!(alternate.output, first.output);
        assert_eq!(alternate.directory.unwrap().sequence, directory.sequence);
    }

    #[test]
    fn agent_shortcut_prompt_survives_transport_chunking_and_rejects_stale_input() {
        let now = Instant::now();
        // Both native SSH and local PTY output enter this same parser. CMD can
        // report A/B without C/D, and PowerShell can terminate OSC with ST.
        for prompt in [
            &b"\x1b]133;A\x07C:\\work>\x1b]133;B\x07"[..],
            &b"\x1b]7;file://localhost/C:/work%20tree\x1b\\\x1b]133;A\x1b\\PS C:\\work tree> \x1b]133;B\x1b\\"[..],
            &b"\x1b]7;file://fedora.test/home/daniel\x07\x1b]133;A\x07daniel@fedora $ \x1b]133;B\x07"[..],
        ] {
            for split in 0..=prompt.len() {
                let mut state = State::default();
                let mut terminal = Terminal::new(100, 24);
                output(&mut state, &mut terminal, &prompt[..split], now);
                if split < prompt.len() {
                    assert!(state.agent_shortcut_prompt().is_none());
                }
                output(&mut state, &mut terminal, &prompt[split..], now);
                let original = state.agent_shortcut_prompt().unwrap();
                state.keyboard(true, b"ordinary", false, now);
                assert!(state.agent_shortcut_prompt().is_none());
                output(&mut state, &mut terminal, b"\x1b]133;B\x07", now);
                assert!(state.agent_shortcut_prompt().is_none());
                state.keyboard(true, b"\r", true, now);
                for byte in prompt {
                    output(&mut state, &mut terminal, &[*byte], now);
                }
                assert_ne!(state.agent_shortcut_prompt().unwrap(), original);
                state.untracked_input();
                assert!(state.agent_shortcut_prompt().is_none());
                output(&mut state, &mut terminal, b"\x1b]133;B\x07", now);
                assert!(state.agent_shortcut_prompt().is_none());
            }
        }
    }

    #[test]
    fn shell_activity_only_confirms_a_literal_typed_cd_after_success_and_prompt() {
        let now = Instant::now();
        let mut state = State::default();
        let mut terminal = Terminal::new(80, 24);
        output(&mut state, &mut terminal, b"\x1b]133;A\x07", now);
        let baseline = state.snapshot();
        for key in [b"c".as_slice(), b"d '/srv/nestd", b"\x7f", b"ed folder'"] {
            state.keyboard(true, key, false, now);
        }
        state.keyboard(true, b"\r", true, now);
        output(
            &mut state,
            &mut terminal,
            b"\x1b]133;C\x07\x1b]133;D;0\x07",
            now,
        );
        assert!(state.snapshot().typed_cd.is_none());
        output(&mut state, &mut terminal, b"\x1b]133;A\x07", now);
        let activity = state.snapshot();
        let typed = activity.typed_cd.unwrap();
        let exit = activity.command_exit.unwrap();
        assert_eq!(&*typed.path, "/srv/nested folder");
        assert_eq!(typed.prompt_before, baseline.prompt);
        assert_eq!(typed.output_before, baseline.output);
        assert_eq!(typed.submitted_at, now);
        assert_eq!(exit.status, Some(0));
        assert!(baseline.prompt < typed.sequence);
        assert!(typed.sequence < exit.sequence && exit.sequence < activity.prompt);
    }

    #[test]
    fn shell_activity_rejects_nonliteral_failed_late_and_untracked_input() {
        for (line, result, delay, untracked) in [
            ("cd /ok; echo false", 0, Duration::ZERO, false),
            ("cd \"/$HOME\"", 0, Duration::ZERO, false),
            ("cd ~", 0, Duration::ZERO, false),
            ("cd lookup", 0, Duration::ZERO, false),
            ("cd /missing", 1, Duration::ZERO, false),
            ("cd /stale", 0, Duration::from_secs(4), false),
            ("cd /not-typed", 0, Duration::ZERO, true),
        ] {
            let now = Instant::now();
            let mut state = State::default();
            let mut terminal = Terminal::new(80, 24);
            output(&mut state, &mut terminal, b"\x1b]133;A\x07", now);
            state.keyboard(true, line.as_bytes(), false, now);
            if untracked {
                state.untracked_input();
            }
            state.keyboard(true, b"\r", true, now);
            output(
                &mut state,
                &mut terminal,
                format!("\x1b]133;C\x07\x1b]133;D;{result}\x07\x1b]133;A\x07").as_bytes(),
                now + delay,
            );
            assert!(state.snapshot().typed_cd.is_none(), "{line}");
        }
    }

    #[test]
    fn shell_activity_prefers_a_real_directory_report_over_typed_evidence() {
        let now = Instant::now();
        let mut state = State::default();
        let mut terminal = Terminal::new(80, 24);
        output(&mut state, &mut terminal, b"\x1b]133;A\x07", now);
        state.keyboard(true, b"cd /logical", false, now);
        state.keyboard(true, b"\r", true, now);
        output(
            &mut state,
            &mut terminal,
            b"\x1b]133;C\x07\x1b]133;D;0\x07\x1b]7;file://example.test/actual\x07\x1b]133;A\x07",
            now,
        );
        let snapshot = state.snapshot();
        assert_eq!(&*snapshot.directory.unwrap().path, "/actual");
        assert!(snapshot.typed_cd.is_none());
    }

    #[test]
    fn command_notification_duration_uses_distinct_start_and_finish_marks() {
        let now = Instant::now();
        let mut state = State::default();
        let mut terminal = Terminal::new(80, 24);
        output(&mut state, &mut terminal, b"\x1b]133;C\x07", now);
        output(
            &mut state,
            &mut terminal,
            b"\x1b]133;D;7\x07",
            now + Duration::from_secs(41),
        );
        let snapshot = state.snapshot();
        assert_eq!(snapshot.completed_runtime, Some(Duration::from_secs(41)));
        assert_eq!(snapshot.command_exit.unwrap().status, Some(7));
        output(
            &mut state,
            &mut terminal,
            b"\x1b]133;D;0\x07",
            now + Duration::from_secs(42),
        );
        assert_eq!(state.snapshot().completed_runtime, None);
    }
}
