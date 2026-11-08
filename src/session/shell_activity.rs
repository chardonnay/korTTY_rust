//! Passive evidence from an SSH session's own terminal and accepted keyboard input.

use std::sync::Arc;
use std::time::{Duration, Instant};

use kortty_terminal::{ShellEvent, ShellEventKind};

const MAX_TYPED_BYTES: usize = 4096;
const TYPED_CD_WINDOW: Duration = Duration::from_secs(3);

#[derive(Clone, Default)]
pub(crate) struct ShellActivity {
    pub(crate) directory: Option<ReportedDirectory>,
    pub(crate) prompt: u64,
    pub(crate) output: u64,
    pub(crate) typed_cd: Option<TypedCd>,
    pub(crate) command_exit: Option<CommandExit>,
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
}

#[derive(Default)]
pub(super) struct State {
    activity: ShellActivity,
    sequence: u64,
    at_prompt: bool,
    ignored_line: bool,
    input: Vec<u8>,
    pending: Option<Pending>,
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

    fn next_sequence(&mut self) -> u64 {
        self.sequence = self.sequence.saturating_add(1);
        self.sequence
    }

    pub(super) fn output(&mut self, primary: bool, events: Vec<ShellEvent>, received_at: Instant) {
        if primary {
            self.activity.output = self.next_sequence();
        }
        for event in events {
            if event.position.alternate_screen {
                continue;
            }
            match event.kind {
                ShellEventKind::WorkingDirectory { host, path } => {
                    self.pending = None;
                    self.activity.typed_cd = None;
                    self.activity.directory = Some(ReportedDirectory {
                        host: host.into(),
                        path: path.into(),
                        sequence: self.next_sequence(),
                    });
                }
                ShellEventKind::PromptStart => {
                    self.activity.prompt = self.next_sequence();
                    self.at_prompt = true;
                    self.ignored_line = false;
                    self.input.clear();
                    if let Some(pending) = self.pending.take()
                        && pending.phase == Phase::Succeeded
                        && pending.fresh_at(received_at)
                    {
                        self.activity.typed_cd = Some(pending.change);
                    }
                }
                ShellEventKind::CommandStart => {}
                ShellEventKind::OutputStart => {
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
                    self.at_prompt = false;
                    self.input.clear();
                    self.activity.command_exit = Some(CommandExit {
                        sequence: self.next_sequence(),
                        status: exit_status,
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
}
