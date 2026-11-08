//! Ordered, output-only manual capture on the terminal actor.

use std::io;
use std::time::{Duration, Instant};

use async_channel::Sender;
use kortty_terminal::{JournalEvent, JournalObserver, JournalRow, Terminal};
use zeroize::{Zeroize as _, Zeroizing};

use crate::session_journal::{
    MAX_RECORD_BYTES, MAX_SEED_BYTES, MAX_SEED_LINES, Metadata, RecordKind, Sink,
};
use crate::terminal_logging::{Policy, RedactionSecrets, Redactor};

const SEED_BUDGET: Duration = Duration::from_millis(32);
const OUTPUT_BUDGET: Duration = Duration::from_millis(8);
const MAX_METADATA_BYTES: usize = 4096;

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
    active: Option<u64>,
}

impl Actor {
    pub(super) fn command(&mut self, terminal: &mut Terminal, command: Command) {
        match command {
            Command::Attach {
                id,
                sink,
                policy,
                seed,
                reply,
            } => {
                let result = self.attach(terminal, id, sink, policy, seed);
                let _ = reply.try_send(result);
            }
            Command::Detach { id, reply } => {
                if self.active == Some(id) {
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
        let redact = Redactor::for_journal(secrets.clone(), policy);
        if seed {
            let (lines, omitted) = seed_lines(terminal, &redact);
            if !lines.is_empty() || omitted != 0 {
                sink.try_seed_batch(lines, omitted).map_err(|_| {
                    io::Error::other("Could not queue the session journal scrollback.")
                })?;
            }
        }
        terminal.set_journal_observer(Some(Box::new(Output {
            sink,
            redact,
            line: Zeroizing::default(),
            deadline: Instant::now(),
            carriage_return: false,
            skip_until_boundary: !terminal.journal_at_line_start(),
            last_blank: false,
            failed: false,
        })));
        self.active = Some(id);
        Ok(())
    }

    pub(super) fn stop(&mut self, terminal: &mut Terminal) {
        if self.active.take().is_some() {
            terminal.set_journal_observer(None);
        }
    }
}

struct Output {
    sink: Sink,
    redact: Redactor,
    line: Zeroizing<String>,
    deadline: Instant,
    carriage_return: bool,
    skip_until_boundary: bool,
    last_blank: bool,
    failed: bool,
}

impl Output {
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
        let line = self.line.trim_end();
        if line.is_empty() && (partial || self.last_blank) {
            self.reset();
            return;
        }
        let Some(text) = self.redact.journal_text(line, self.deadline) else {
            self.truncate();
            return;
        };
        self.last_blank = text.is_empty();
        self.reset();
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
        self.deadline = Instant::now() + OUTPUT_BUDGET;
    }

    fn observe(&mut self, event: JournalEvent) {
        if self.failed {
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
