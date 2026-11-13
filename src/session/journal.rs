//! Ordered journal capture on the terminal actor.

use std::io;
use std::sync::mpsc::SyncSender;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use async_channel::Sender;
use kortty_terminal::{
    JournalEvent, JournalObserver, JournalRow, ShellEvent, ShellEventKind, Terminal,
};
use zeroize::{Zeroize as _, Zeroizing};

use crate::session_journal::{
    MAX_RECORD_BYTES, MAX_SEED_BYTES, MAX_SEED_LINES, Metadata, RecordKind, Sink,
};
use crate::terminal_logging::{Policy, RedactionSecrets, Redactor};

const SEED_BUDGET: Duration = Duration::from_millis(32);
const OUTPUT_BUDGET: Duration = Duration::from_millis(8);
const MAX_METADATA_BYTES: usize = 4096;
mod input;
#[cfg(test)]
#[path = "journal/tests.rs"]
mod input_capture_tests;

pub(super) fn redact_metadata(
    mut metadata: Metadata,
    secrets: RedactionSecrets,
    policy: &Policy,
) -> Option<Metadata> {
    let redact = Redactor::for_journal(secrets, policy.clone());
    let deadline = Instant::now() + OUTPUT_BUDGET;
    for field in [
        &mut metadata.title,
        &mut metadata.connection_name,
        &mut metadata.host,
        &mut metadata.username,
    ] {
        let raw = Zeroizing::new(std::mem::take(field));
        if raw.len() > MAX_METADATA_BYTES {
            return None;
        }
        let mut text = redact.journal_text(&raw, deadline)?;
        if text.len() > MAX_METADATA_BYTES {
            return None;
        }
        *field = std::mem::take(&mut *text);
    }
    Some(metadata)
}

pub(super) enum Command {
    Attach {
        id: u64,
        sink: Sink,
        policy: Policy,
        seed: bool,
        capture_input: bool,
        reply: Sender<io::Result<()>>,
    },
    Detach {
        id: u64,
        reply: Sender<()>,
    },
}

impl Command {
    pub(super) fn unavailable(self) {
        match self {
            Self::Attach { reply, .. } => {
                let _ = reply.try_send(Err(io::Error::other(
                    "Session journal capture is unavailable.",
                )));
            }
            Self::Detach { reply, .. } => {
                let _ = reply.try_send(());
            }
        }
    }
}

#[derive(Default)]
pub(super) struct Actor {
    pub(super) secrets: Option<RedactionSecrets>,
    active: Option<Active>,
}

struct Active {
    id: u64,
    sink: Sink,
    input: Option<Input>,
}

struct Input {
    redact: Redactor,
    lines: input::Lines,
    prompt: Option<u64>,
}

pub(super) struct Permit {
    id: u64,
    prompt: u64,
    pub(super) output: Option<Instant>,
    pub(super) confirmation: Option<SyncSender<()>>,
    encoding: kortty_terminal::TextEncoding,
}

impl Actor {
    pub(super) fn command(
        &mut self,
        terminal: &mut Terminal,
        command: Command,
        prompt: Option<u64>,
    ) {
        match command {
            Command::Attach {
                id,
                sink,
                policy,
                seed,
                capture_input,
                reply,
            } => {
                let result = self.attach(terminal, id, sink, policy, seed, capture_input);
                if result.is_ok() {
                    self.bind_existing_prompt(prompt);
                }
                let _ = reply.try_send(result);
            }
            Command::Detach { id, reply } => {
                if self.active.as_ref().is_some_and(|active| active.id == id) {
                    self.stop(terminal);
                }
                let _ = reply.try_send(());
            }
        }
    }

    fn attach(
        &mut self,
        terminal: &mut Terminal,
        id: u64,
        sink: Sink,
        policy: Policy,
        seed: bool,
        capture_input: bool,
    ) -> io::Result<()> {
        let secrets = self
            .secrets
            .as_ref()
            .filter(|_| terminal.supports_journal_output())
            .ok_or_else(|| io::Error::other("Session journal capture is unavailable."))?;
        if self.active.is_some() {
            return Err(io::Error::other(
                "This terminal already has an attached session journal.",
            ));
        }
        let input = capture_input.then(|| Input {
            redact: Redactor::for_journal(secrets.clone(), policy.clone()),
            lines: input::Lines::default(),
            prompt: None,
        });
        let redact = Redactor::for_journal(secrets.clone(), policy);
        if seed {
            let (lines, omitted) = seed_lines(terminal, &redact);
            if !lines.is_empty() || omitted != 0 {
                sink.try_seed_batch(lines, omitted).map_err(|_| {
                    io::Error::other("Could not queue the session journal scrollback.")
                })?;
            }
        }
        terminal.set_journal_observer(Some(journal_output_observer(
            sink.clone(),
            Arc::new(redact),
            "",
            !terminal.journal_at_line_start(),
            None,
        )));
        self.active = Some(Active { id, sink, input });
        Ok(())
    }

    pub(super) fn captures_input(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| active.input.is_some())
    }

    pub(super) fn suppress_input(&mut self) {
        if let Some(input) = self
            .active
            .as_mut()
            .and_then(|active| active.input.as_mut())
        {
            input.lines.suppress();
        }
    }

    fn bind_existing_prompt(&mut self, prompt: Option<u64>) {
        if let Some(prompt) = prompt
            && let Some(input) = self
                .active
                .as_mut()
                .and_then(|active| active.input.as_mut())
        {
            input.prompt = Some(prompt);
            input.lines.suppress();
        }
    }

    pub(super) fn permit(
        &mut self,
        terminal: &Terminal,
        prompt: Option<u64>,
        output: Option<Instant>,
        coherent: bool,
        bytes: &[u8],
        enter: bool,
    ) -> Option<Permit> {
        let active = self.active.as_mut()?;
        let input = active.input.as_mut()?;
        if prompt.is_some() && input.prompt != prompt {
            input.lines.reset();
            input.prompt = prompt;
        }
        let Some(prompt) = prompt.filter(|_| {
            coherent
                && !terminal.alternate_screen_active()
                && !terminal.cursor_line_is_password_prompt()
        }) else {
            input.lines.suppress();
            return None;
        };
        let confirmation = if enter {
            input
                .lines
                .written(bytes, true)
                .and_then(|line| {
                    input
                        .redact
                        .journal_text(&line, Instant::now() + OUTPUT_BUDGET)
                })
                .and_then(|text| active.sink.reserve_input(text).ok())
        } else {
            None
        };
        Some(Permit {
            id: active.id,
            prompt,
            output,
            confirmation,
            encoding: terminal.journal_input_encoding(),
        })
    }

    pub(super) fn missed_input(&mut self, permit: Permit) {
        if let Some(active) = self.active.as_mut().filter(|active| active.id == permit.id)
            && let Some(input) = active
                .input
                .as_mut()
                .filter(|input| input.prompt == Some(permit.prompt))
        {
            input.lines.suppress();
        }
    }

    pub(super) fn written(&mut self, permit: Permit, bytes: &[u8]) {
        let Some(active) = self.active.as_mut().filter(|active| active.id == permit.id) else {
            return;
        };
        let Some(input) = active
            .input
            .as_mut()
            .filter(|input| input.prompt == Some(permit.prompt))
        else {
            return;
        };
        if permit.encoding == kortty_terminal::TextEncoding::Utf8 {
            let _ = input.lines.written(bytes, false);
        } else {
            let text = Zeroizing::new(permit.encoding.decode(bytes).into_owned());
            if text.contains(char::REPLACEMENT_CHARACTER) {
                input.lines.suppress();
            } else {
                let _ = input.lines.written(text.as_bytes(), false);
            }
        }
    }

    pub(super) fn output(&mut self, terminal: &Terminal, events: &[ShellEvent]) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        if let Some(input) = active.input.as_mut()
            && (terminal.alternate_screen_active() || terminal.cursor_line_is_password_prompt())
        {
            input.lines.suppress();
        }
        for event in events {
            if let ShellEventKind::CommandFinished { exit_status } = event.kind
                && !event.position.alternate_screen
            {
                let text = exit_status.map_or_else(
                    || "shell integration: command finished".to_owned(),
                    |status| format!("shell integration: command finished (exit {status})"),
                );
                let _ = active
                    .sink
                    .try_write(RecordKind::CommandEnd, Zeroizing::new(text), false);
            }
        }
    }

    pub(super) fn stop(&mut self, terminal: &mut Terminal) {
        if self.active.take().is_some() {
            terminal.set_journal_observer(None);
        }
    }
}

struct Output {
    sink: Sink,
    redact: Arc<Redactor>,
    prefix: &'static str,
    stopped: Option<Arc<AtomicBool>>,
    line: Zeroizing<String>,
    deadline: Instant,
    carriage_return: bool,
    skip_until_boundary: bool,
    last_blank: bool,
    failed: bool,
}

pub(crate) fn journal_output_observer(
    sink: Sink,
    redact: Arc<Redactor>,
    prefix: &'static str,
    skip_until_boundary: bool,
    stopped: Option<Arc<AtomicBool>>,
) -> Box<dyn JournalObserver> {
    Box::new(Output {
        sink,
        redact,
        prefix,
        stopped,
        line: Zeroizing::default(),
        deadline: Instant::now(),
        carriage_return: false,
        skip_until_boundary,
        last_blank: false,
        failed: false,
    })
}

impl Output {
    fn stopped(&self) -> bool {
        self.stopped
            .as_ref()
            .is_some_and(|stop| stop.load(Ordering::Acquire))
    }
    fn reset(&mut self) {
        self.line.zeroize();
        self.carriage_return = false;
    }

    fn truncate(&mut self) {
        self.failed = true;
        self.reset();
        self.sink.mark_truncated();
    }

    fn print(&mut self, character: char) {
        if self.carriage_return {
            self.reset();
        }
        if self.line.len() + character.len_utf8() > MAX_RECORD_BYTES {
            self.truncate();
        } else {
            self.line.push(character);
        }
    }

    fn emit(&mut self, partial: bool) {
        if self.stopped() {
            self.reset();
            return;
        }
        let line = self.line.trim_end();
        if line.is_empty() && (partial || self.last_blank) {
            self.reset();
            return;
        }
        let Some(mut text) = self.redact.journal_text(line, self.deadline) else {
            self.truncate();
            return;
        };
        self.last_blank = text.is_empty();
        self.reset();
        if !text.is_empty() {
            text.insert_str(0, self.prefix);
        }
        if self
            .sink
            .try_write(RecordKind::Output, text, partial)
            .is_err()
        {
            self.failed = true;
        }
    }
}

impl JournalObserver for Output {
    fn begin_chunk(&mut self) {
        if self.stopped() || self.sink.output_paused() {
            self.reset();
            return;
        }
        self.deadline = Instant::now() + OUTPUT_BUDGET;
    }

    fn observe(&mut self, event: JournalEvent) {
        if self.failed || self.stopped() || self.sink.output_paused() {
            return;
        }
        if self.skip_until_boundary {
            if matches!(event, JournalEvent::LineFeed | JournalEvent::Boundary) {
                self.skip_until_boundary = false;
            }
            return;
        }
        match event {
            JournalEvent::Print(character) => self.print(character),
            JournalEvent::Tab => self.print('\t'),
            JournalEvent::LineFeed => self.emit(false),
            JournalEvent::CarriageReturn => self.carriage_return = true,
            JournalEvent::Backspace => {
                self.line.pop();
            }
            JournalEvent::Boundary => self.reset(),
        }
    }

    fn finish(&mut self) {
        if !self.failed && !self.line.is_empty() {
            self.begin_chunk();
            self.emit(true);
        }
    }
}

fn seed_lines(terminal: &Terminal, redact: &Redactor) -> (Vec<Zeroizing<String>>, usize) {
    let deadline = Instant::now() + SEED_BUDGET;
    let rows = terminal.journal_rows();
    let total = rows.len();
    let mut omitted = total;
    let mut lines = Vec::new();
    let mut bytes = 0;
    let mut pending = Zeroizing::<String>::default();
    let mut pending_rows = 0;
    for (index, row) in rows.rev().take(MAX_SEED_LINES).enumerate() {
        if Instant::now() >= deadline {
            break;
        }
        if row.wrapped_columns.is_none() && pending_rows != 0 {
            if !add_seed(&mut lines, &mut bytes, &pending, redact, deadline) {
                break;
            }
            omitted -= pending_rows;
            pending_rows = 0;
            pending.zeroize();
        }
        let Some(mut text) = seed_row(row) else {
            break;
        };
        if lines.is_empty() && pending_rows == 0 && text.trim().is_empty() {
            omitted -= 1;
            continue;
        }
        if text.len() + pending.len() > MAX_RECORD_BYTES {
            break;
        }
        text.push_str(&pending);
        pending = text;
        pending_rows += 1;
        if index + 1 == total && add_seed(&mut lines, &mut bytes, &pending, redact, deadline) {
            omitted -= pending_rows;
        }
    }
    lines.reverse();
    (lines, omitted)
}

fn seed_row(row: JournalRow<'_>) -> Option<Zeroizing<String>> {
    let columns = row.wrapped_columns.unwrap_or(row.cells.len());
    let mut text = Zeroizing::<String>::default();
    let mut spaces = 0;
    for cell in row.cells.iter().take(columns) {
        if cell.width == 0 {
            continue;
        }
        if cell.style.hidden || cell.text.is_empty() {
            spaces += usize::from(cell.width);
            continue;
        }
        if text.len() + spaces + cell.text.len() > MAX_RECORD_BYTES {
            return None;
        }
        text.extend(std::iter::repeat_n(' ', spaces));
        text.extend(
            cell.text
                .chars()
                .filter(|character| !character.is_control()),
        );
        spaces = 0;
    }
    if row.wrapped_columns.is_some() {
        if text.len() + spaces > MAX_RECORD_BYTES {
            return None;
        }
        text.extend(std::iter::repeat_n(' ', spaces));
    }
    Some(text)
}

fn add_seed(
    lines: &mut Vec<Zeroizing<String>>,
    bytes: &mut usize,
    line: &str,
    redact: &Redactor,
    deadline: Instant,
) -> bool {
    let Some(text) = redact.journal_text(line.trim_end(), deadline) else {
        return false;
    };
    if text.len() > MAX_RECORD_BYTES || *bytes + text.len() > MAX_SEED_BYTES {
        return false;
    }
    *bytes += text.len();
    lines.push(text);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_logging::Secrets;

    #[test]
    fn scrollback_seed_redacts_a_known_secret_across_soft_wraps() {
        let secrets = Secrets::default();
        secrets.add("wrapped-secret");
        let redactor = Redactor::for_journal(secrets.seal().unwrap(), Policy::default());
        let mut terminal = Terminal::new(5, 3);
        terminal.advance(b"wrapped-secret\r\nvisible\r\nunfinished-tail");
        let (seed, omitted) = seed_lines(&terminal, &redactor);
        assert_eq!(omitted, 0);
        assert_eq!(
            seed.iter().map(|line| line.as_str()).collect::<Vec<_>>(),
            ["***", "visible"]
        );
    }
}
