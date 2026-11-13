//! Command boundaries and bounded evidence for journal summaries.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::sync::LazyLock;
use std::time::Duration;

use jiff::Timestamp;
use regex::Regex;
use rsa::signature::digest::DynDigest;
use serde::{Serialize, Serializer};
use zeroize::Zeroizing;

use super::{CaptureKind, CaptureRecord, Cursor, is_display_character};

pub(super) mod filters;
mod replay;
pub(super) use replay::LongOutput;

const LOOKAHEAD: usize = 3;
const MAX_SOURCE_LINE_BYTES: usize = 4096;
const MAX_PROMPT_BYTES: usize = 192;
const MAX_PROMPTS: usize = 16;
const HEAD: usize = 7;
const NOTABLE: usize = 7;
const TAIL: usize = 10;
const REPLY_CONTEXT: usize = 3;
const MAX_REPLY_CONTEXT: usize = 6;
pub(super) const SAMPLE_LINE_BYTES: usize = 384;
pub(super) const COMMAND_FINISHED: &str = "shell integration: command finished";
const RUNNING_AFTER: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum End {
    Complete,
    Running,
    SessionEnded,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct Command {
    #[serde(skip)]
    pub(crate) sequence: u64,
    pub(crate) text: String,
    pub(crate) exit: Option<i32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    Output,
    Input,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct Line {
    pub(crate) sequence: u64,
    pub(crate) kind: Kind,
    pub(crate) text: String,
    pub(crate) partial: bool,
    pub(crate) repeat: u32,
}

#[derive(Clone, Serialize)]
pub(crate) struct Window {
    pub(crate) first: u64,
    pub(crate) last: u64,
    #[serde(serialize_with = "timestamp")]
    pub(crate) started: Timestamp,
    #[serde(serialize_with = "timestamp")]
    pub(crate) through: Timestamp,
    pub(crate) end: End,
    pub(crate) commands: Vec<Command>,
    pub(crate) evidence: Vec<Line>,
    pub(crate) omitted: u64,
    #[serde(skip)]
    pub(super) skipped: bool,
    #[serde(skip)]
    pub(super) long_output: Option<LongOutput>,
}

fn timestamp<S: Serializer>(value: &Timestamp, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(value)
}

impl Window {
    pub(crate) fn checkpoint(&self) -> Option<u64> {
        (self.end != End::Running).then_some(self.last)
    }
}

#[derive(Default)]
struct Evidence {
    first: Vec<Line>,
    notable: Vec<Line>,
    reply_context: Vec<Line>,
    tail: VecDeque<Line>,
    total: u64,
    clipped: bool,
}

impl Evidence {
    fn record(&mut self, mut line: Line) {
        self.total = self.total.saturating_add(1);
        self.clipped |= line.text.len() > SAMPLE_LINE_BYTES;
        if line.text.len() > MAX_SOURCE_LINE_BYTES {
            line.text = "(long capture record omitted)".into();
        }
        if line.kind == Kind::Input && self.total > 1 {
            for prior in self.tail.iter().rev().take(REPLY_CONTEXT) {
                if self.reply_context.len() == MAX_REPLY_CONTEXT {
                    break;
                }
                if prior.kind == Kind::Output
                    && !self
                        .reply_context
                        .iter()
                        .any(|line| line.sequence == prior.sequence)
                {
                    self.reply_context.push(prior.clone());
                }
            }
        }
        if self.first.len() < HEAD {
            self.first.push(line);
            return;
        }
        if self.notable.len() < NOTABLE && is_notable(&line.text) {
            self.notable.push(line.clone());
        }
        if self.tail.len() == TAIL {
            self.tail.pop_front();
        }
        self.tail.push_back(line);
    }

    fn snapshot(&self) -> (Vec<Line>, u64) {
        let by_sequence: BTreeMap<_, _> = self
            .first
            .iter()
            .chain(&self.notable)
            .chain(&self.reply_context)
            .chain(&self.tail)
            .map(|line| (line.sequence, line.clone()))
            .collect();
        let omitted = self.total.saturating_sub(by_sequence.len() as u64);
        (by_sequence.into_values().collect(), omitted)
    }
}

struct Block {
    first: u64,
    last: u64,
    started: Timestamp,
    through: Timestamp,
    command: Option<Command>,
    evidence: Evidence,
    idle: bool,
    help_heading: bool,
    output_lines: usize,
    failed_start: bool,
    replay: Option<replay::Builder>,
}

impl Block {
    fn new(record: &Observation, command: Option<String>) -> Self {
        Self {
            first: record.sequence,
            last: record.sequence,
            started: record.timestamp,
            through: record.timestamp,
            command: command.map(|text| Command {
                sequence: record.sequence,
                text,
                exit: None,
            }),
            evidence: Evidence::default(),
            idle: true,
            help_heading: false,
            output_lines: 0,
            failed_start: false,
            replay: record
                .replay
                .as_ref()
                .map(|start| replay::Builder::new(Arc::clone(start))),
        }
    }

    fn progress(&mut self, record: &Observation) {
        self.last = record.sequence;
        self.through = self.through.max(record.timestamp);
        if let Some(replay) = &mut self.replay {
            replay.record(record);
        }
    }

    fn record(&mut self, record: Observation, kind: Kind) {
        self.progress(&record);
        let idle = match kind {
            Kind::Output => filters::idle(&record.text),
            Kind::Input => record.text.trim().is_empty(),
        };
        self.idle &= idle;
        if kind == Kind::Output && !record.partial && !record.redacted && !idle {
            self.output_lines += 1;
            self.help_heading |= self.output_lines <= 40 && filters::help_heading(&record.text);
            self.failed_start |= self.evidence.total <= 6
                && self
                    .command
                    .as_ref()
                    .is_some_and(|command| filters::failed_start(&command.text, &record.text));
        }
        self.evidence.record(record.line(kind));
    }

    fn window(&self, end: End) -> Window {
        let (evidence, omitted) = self.evidence.snapshot();
        let skipped = self.idle
            || self.command.as_ref().is_some_and(|command| {
                filters::help_only(&command.text, self.help_heading)
                    || self.failed_start && command.exit != Some(0)
            });
        let long_output = if !skipped
            && end != End::Running
            && self.command.is_some()
            && (omitted > 0 || self.evidence.clipped)
        {
            self.replay.as_ref().map(replay::Builder::seal)
        } else {
            None
        };
        Window {
            first: self.first,
            last: self.last,
            started: self.started,
            through: self.through,
            end,
            commands: self.command.iter().cloned().collect(),
            evidence,
            omitted,
            skipped,
            long_output,
        }
    }
}

enum ObservationKind {
    Line(Kind),
    Finished(Option<i32>),
}

struct Observation {
    sequence: u64,
    timestamp: Timestamp,
    kind: ObservationKind,
    text: String,
    partial: bool,
    redacted: bool,
    repeat: u32,
    replay: Option<Arc<replay::Start>>,
}

impl Observation {
    fn from_record(record: CaptureRecord, preserve_long: bool) -> Option<Self> {
        let kind = match record.kind {
            CaptureKind::Output | CaptureKind::Seed => ObservationKind::Line(Kind::Output),
            CaptureKind::Input => ObservationKind::Line(Kind::Input),
            CaptureKind::Note if !record.redacted && !record.partial => {
                ObservationKind::Finished(exit_status(&record.text)?)
            }
            CaptureKind::Note | CaptureKind::Screenshot => return None,
        };
        // A hidden input record stays hidden even when an imported log contains unexpected text.
        let oversized = record.text.len() > MAX_SOURCE_LINE_BYTES;
        let text = if record.redacted && matches!(kind, ObservationKind::Line(Kind::Input)) {
            "(hidden input)".into()
        } else if oversized && !preserve_long {
            // Keep the complete raw record in the capture. Sending a clipped prefix to an
            // AI profile could evade a credential mask that needs the rest of the text.
            "(long capture record omitted)".into()
        } else {
            record
                .text
                .chars()
                .filter(|&ch| is_display_character(ch))
                .collect()
        };
        Some(Self {
            sequence: record.sequence,
            timestamp: record.timestamp,
            kind,
            text,
            partial: record.partial,
            redacted: record.redacted || oversized,
            repeat: record.repeat,
            replay: None,
        })
    }

    fn line(self, kind: Kind) -> Line {
        Line {
            sequence: self.sequence,
            kind,
            text: self.text,
            partial: self.partial,
            repeat: self.repeat,
        }
    }

    fn fingerprint(&self, hash: &mut dyn DynDigest) {
        hash.update(&self.sequence.to_le_bytes());
        hash.update(&self.timestamp.as_second().to_le_bytes());
        hash.update(&self.timestamp.subsec_nanosecond().to_le_bytes());
        let kind = match self.kind {
            ObservationKind::Line(Kind::Output) => 0,
            ObservationKind::Line(Kind::Input) => 1,
            ObservationKind::Finished(_) => 2,
        };
        hash.update(&[kind, u8::from(self.partial), u8::from(self.redacted)]);
        hash.update(&self.repeat.to_le_bytes());
        hash.update(&(self.text.len() as u64).to_le_bytes());
        hash.update(self.text.as_bytes());
    }
}

/// Feed pages in physical order. A caller can persist each emitted checkpoint independently.
/// Lookahead and the still-running command retain bounded evidence even across long captures.
pub(crate) struct Planner {
    newest: u64,
    incoming: VecDeque<Observation>,
    block: Option<Block>,
    prompt_line: Option<(String, bool)>,
    prompts: Vec<String>,
    shell_finished: bool,
    sensitive: SensitiveBlocks,
    replay: Option<Arc<replay::Start>>,
}

impl Planner {
    pub(crate) fn resume(after: u64) -> Self {
        Self {
            newest: after,
            incoming: VecDeque::with_capacity(LOOKAHEAD + 1),
            block: None,
            prompt_line: None,
            prompts: Vec::new(),
            // A persisted checkpoint ends before the next command starts.
            shell_finished: true,
            sensitive: SensitiveBlocks::default(),
            replay: None,
        }
    }

    pub(crate) fn capture_gap(&mut self) {
        self.sensitive.uncertain = true;
    }

    pub(super) fn begin_page(&mut self, cursor: Option<Cursor>, full_output: bool) {
        self.replay = full_output.then(|| {
            Arc::new(replay::Start {
                cursor,
                sensitive: self.sensitive.clone(),
            })
        });
    }

    pub(crate) fn push(&mut self, record: CaptureRecord) -> Option<Window> {
        // Interpret sensitive boundaries in physical order, even before a saved checkpoint.
        let record = self.sensitive.mask(record);
        let mut observation = Observation::from_record(record, self.replay.is_some())?;
        if observation.sequence <= self.newest {
            return None;
        }
        observation.replay.clone_from(&self.replay);
        self.newest = observation.sequence;
        self.incoming.push_back(observation);
        (self.incoming.len() > LOOKAHEAD)
            .then(|| self.process_one())
            .flatten()
    }

    /// Flush ready output for this periodic pass. A running card is a replaceable preview and
    /// carries no durable checkpoint. Closing turns the unfinished tail into permanent evidence.
    pub(crate) fn finish_pass(
        &mut self,
        closing: bool,
        quiet: bool,
        now: Timestamp,
    ) -> Vec<Window> {
        let mut ready = Vec::new();
        while !self.incoming.is_empty() {
            ready.extend(self.process_one());
        }
        let returned_to_prompt = self
            .prompt_line
            .as_ref()
            .is_some_and(|(line, _)| self.prompts.iter().any(|prompt| prompt == line.trim()));
        if (closing
            || returned_to_prompt
            || (quiet
                && self
                    .block
                    .as_ref()
                    .is_some_and(|block| block.command.is_none())))
            && let Some(block) = self.block.take()
        {
            let end = if block.command.is_some() && !returned_to_prompt {
                End::SessionEnded
            } else {
                End::Complete
            };
            ready.push(block.window(end));
            self.shell_finished = true;
        }
        if !closing
            && let Some(block) = &self.block
            && block.command.is_some()
            && now.as_second().saturating_sub(block.started.as_second())
                >= RUNNING_AFTER.as_secs() as i64
        {
            let preview = block.window(End::Running);
            if !preview.skipped {
                ready.push(preview);
            }
        }
        ready
    }

    fn process_one(&mut self) -> Option<Window> {
        let record = self
            .incoming
            .pop_front()
            .expect("buffered capture evidence");
        let kind = match record.kind {
            ObservationKind::Finished(exit) => {
                self.shell_finished = true;
                let mut block = self.block.take()?;
                block.progress(&record);
                if let Some(command) = &mut block.command {
                    command.exit = exit;
                }
                return Some(block.window(End::Complete));
            }
            ObservationKind::Line(kind) => kind,
        };
        let mut ready = None;
        let typed = kind == Kind::Input
            && !record.redacted
            && !record.partial
            && !record.text.trim().is_empty();
        let prompt = typed
            .then(|| self.command_prompt(&record.text).map(str::to_owned))
            .flatten();
        if typed && (self.shell_finished || prompt.is_some()) {
            if let Some(block) = self.block.take() {
                ready = Some(block.window(End::Complete));
            }
            if let Some(prompt) = prompt
                && self.prompts.len() < MAX_PROMPTS
                && !self.prompts.contains(&prompt)
            {
                self.prompts.push(prompt);
            }
            self.block = Some(Block::new(&record, Some(record.text.trim().to_owned())));
            self.prompt_line = None;
            self.shell_finished = false;
        }
        if kind == Kind::Output {
            self.prompt_line = Some((record.text.clone(), record.partial));
        }
        let block = self.block.get_or_insert_with(|| Block::new(&record, None));
        block.record(record, kind);
        ready
    }

    fn command_prompt(&self, typed: &str) -> Option<&str> {
        self.prompt_line
            .as_ref()
            .and_then(|(line, partial)| prompt_before(line, typed, *partial))
            .or_else(|| {
                self.incoming.iter().find_map(|next| {
                    if matches!(next.kind, ObservationKind::Line(Kind::Output))
                        && next.text.trim_end().ends_with(typed.trim())
                    {
                        prompt_before(&next.text, typed, next.partial)
                    } else {
                        None
                    }
                })
            })
    }
}

#[derive(Clone, Default)]
pub(super) struct SensitiveBlocks {
    inside: bool,
    uncertain: bool,
    tail: Zeroizing<String>,
}

impl SensitiveBlocks {
    fn mask(&mut self, mut record: CaptureRecord) -> CaptureRecord {
        if matches!(
            record.kind,
            CaptureKind::Input | CaptureKind::Output | CaptureKind::Seed
        ) && self.excludes(&record.text)
        {
            record.text = "(sensitive capture record hidden)".into();
            record.redacted = true;
        }
        record
    }

    pub(super) fn excludes(&mut self, text: &str) -> bool {
        const MARKER: &str = "-----";
        const KEY: &str = "PRIVATE KEY";
        const TAIL_BYTES: usize = 160;
        static BOUNDARY: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"-----(?:BEGIN |END )").expect("static private-key boundary")
        });
        if self.uncertain {
            return true;
        }
        let mut excluded = self.inside;
        for line in text.split('\n') {
            let previous = self.tail.len();
            let mut context = Zeroizing::new(std::mem::take(&mut *self.tail));
            context.push_str(line);
            context.make_ascii_uppercase();
            for boundary in BOUNDARY.find_iter(&context) {
                let label_start = boundary.end();
                let begins = boundary.as_str().ends_with("BEGIN ");
                let rest = &context[label_start..];
                let end = rest.find(MARKER);
                let label = &rest[..end.unwrap_or(rest.len())];
                if let Some(key) = label.find(KEY) {
                    if begins && label_start + key + KEY.len() > previous {
                        self.inside = true;
                        excluded = true;
                    }
                    if !begins
                        && let Some(end) = end
                        && label_start + end + MARKER.len() > previous
                    {
                        self.inside = false;
                        excluded = true;
                    }
                }
                // Suppress the first fragment of a split marker as well as its continuation.
                if end.is_none() && label_start > previous && begins {
                    excluded = true;
                }
            }
            if let Some((_, suffix)) = context.rsplit_once(MARKER)
                && context.len() > previous
                && suffix.starts_with('B')
                && "BEGIN ".starts_with(suffix)
            {
                excluded = true;
            }
            let start = context.ceil_char_boundary(context.len().saturating_sub(TAIL_BYTES));
            self.tail.push_str(&context[start..]);
        }
        excluded
    }
}

fn exit_status(text: &str) -> Option<Option<i32>> {
    let tail = text.strip_prefix(COMMAND_FINISHED)?;
    if tail.is_empty() {
        return Some(None);
    }
    Some(Some(
        tail.strip_prefix(" (exit ")?
            .strip_suffix(')')?
            .parse()
            .ok()?,
    ))
}

fn prompt_before<'a>(line: &'a str, typed: &str, partial: bool) -> Option<&'a str> {
    let line = line.trim_end();
    let prefix = line.strip_suffix(typed.trim()).map(str::trim_end);
    if !partial && prefix.is_none() {
        return None;
    }
    let prompt = prefix.unwrap_or(line).trim();
    if prompt.is_empty() || prompt.len() > MAX_PROMPT_BYTES {
        return None;
    }
    let mut chars = prompt.chars().rev();
    let ending = chars.next().expect("nonempty prompt");
    ("$#%>❯➜»λ".contains(ending)
        && chars.next().is_none_or(|previous| {
            previous != ending && !(ending == '%' && previous.is_ascii_digit())
        }))
    .then_some(prompt)
}

fn is_notable(line: &str) -> bool {
    line.split(|ch: char| !ch.is_alphanumeric()).any(|word| {
        [
            "error",
            "failed",
            "failure",
            "fatal",
            "warning",
            "denied",
            "refused",
            "exception",
            "traceback",
            "timeout",
        ]
        .iter()
        .any(|notable| word.eq_ignore_ascii_case(notable))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(sequence: u64, kind: CaptureKind, text: &str) -> CaptureRecord {
        CaptureRecord {
            sequence,
            timestamp: Timestamp::from_second(sequence as i64).unwrap(),
            kind,
            text: text.to_owned(),
            redacted: false,
            partial: false,
            repeat: 1,
        }
    }

    #[test]
    fn command_blocks_distinguish_prompts_replies_and_shell_exit_status() {
        let mut planner = Planner::resume(0);
        let mut ready = Vec::new();
        for (index, (kind, text)) in [
            (CaptureKind::Output, "fixture@host:~$"),
            (CaptureKind::Input, "configure"),
            (CaptureKind::Output, "Proceed? (y/n)"),
            (CaptureKind::Input, "y"),
            (
                CaptureKind::Note,
                "shell integration: command finished (exit 2)",
            ),
            (CaptureKind::Input, "verify"),
            (CaptureKind::Output, "working"),
        ]
        .into_iter()
        .enumerate()
        {
            ready.extend(planner.push(record(index as u64 + 1, kind, text)));
        }
        assert_eq!(ready.len(), 1);
        assert!(ready[0].skipped);
        assert_eq!(ready[0].checkpoint(), Some(1));
        let windows = planner.finish_pass(false, false, Timestamp::from_second(10).unwrap());
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].checkpoint(), Some(5));
        assert_eq!(
            windows[0].commands,
            [Command {
                sequence: 2,
                text: "configure".into(),
                exit: Some(2)
            }]
        );
        let windows = planner.finish_pass(false, true, Timestamp::from_second(1_000).unwrap());
        assert_eq!(windows[0].end, End::Running);
        assert_eq!(windows[0].checkpoint(), None);
        let windows = planner.finish_pass(true, true, Timestamp::from_second(1_001).unwrap());
        assert_eq!(windows[0].end, End::SessionEnded);
        assert_eq!(windows[0].checkpoint(), Some(7));
    }

    #[test]
    fn journal_summary_keeps_one_card_per_command_and_skips_help_and_failed_starts() {
        let mut planner = Planner::resume(0);
        let mut ready = Vec::new();
        for (index, (command, output, exit)) in [
            ("ls --help", "Usage: ls [OPTION]", 0),
            ("man crontab", "CRONTAB(5)", 0),
            ("dnf -h", "Verwendung: dnf", 0),
            ("lnks notes", "-bash: lnks: command not found", 127),
            ("sudo -E X=1 lnks", "sudo: lnks: command not found", 1),
            ("df -h", "Filesystem Size Used Avail", 0),
            ("cat notes", "cat: notes: No such file or directory", 1),
            ("ls --help | head", "Usage: ls [OPTION]", 0),
        ]
        .into_iter()
        .enumerate()
        {
            let first = index as u64 * 3 + 1;
            ready.extend(planner.push(record(first, CaptureKind::Input, command)));
            ready.extend(planner.push(record(first + 1, CaptureKind::Output, output)));
            ready.extend(planner.push(record(
                first + 2,
                CaptureKind::Note,
                &format!("{COMMAND_FINISHED} (exit {exit})"),
            )));
        }
        ready.extend(planner.finish_pass(true, true, Timestamp::from_second(100).unwrap()));
        assert_eq!(ready.len(), 8);
        assert!(ready[..5].iter().all(|window| window.skipped));
        assert_eq!(ready.iter().filter(|window| !window.skipped).count(), 3);
        for (index, window) in ready.iter().enumerate() {
            assert_eq!(window.commands.len(), 1);
            assert_eq!(window.checkpoint(), Some((index as u64 + 1) * 3));
        }
        assert_eq!(ready[5].commands[0].text, "df -h");
        assert_eq!(ready[6].commands[0].exit, Some(1));
    }

    #[test]
    fn journal_summary_leaves_a_running_help_command_private_until_checkpoint() {
        let mut planner = Planner::resume(0);
        assert!(
            planner
                .push(record(1, CaptureKind::Input, "man crontab"))
                .is_none()
        );
        assert!(
            planner
                .push(record(2, CaptureKind::Output, "CRONTAB(5)"))
                .is_none()
        );
        let now = Timestamp::from_second(1_000).unwrap();
        assert!(planner.finish_pass(false, false, now).is_empty());
        let closing = planner.finish_pass(true, false, now);
        assert_eq!(closing.len(), 1);
        assert!(closing[0].skipped);
        assert_eq!(closing[0].checkpoint(), Some(2));
    }

    #[test]
    fn restart_skips_finalized_and_duplicate_records_and_bounds_long_evidence() {
        let mut planner = Planner::resume(40);
        assert!(
            planner
                .push(record(40, CaptureKind::Output, "old"))
                .is_none()
        );
        for sequence in 41..=240 {
            let text = if sequence == 150 {
                "ERROR retained"
            } else {
                "routine"
            };
            assert!(
                planner
                    .push(record(sequence, CaptureKind::Output, text))
                    .is_none()
            );
            assert!(
                planner
                    .push(record(sequence, CaptureKind::Output, "duplicate"))
                    .is_none()
            );
        }
        let windows = planner.finish_pass(true, true, Timestamp::from_second(250).unwrap());
        let window = &windows[0];
        assert_eq!((window.first, window.last), (41, 240));
        assert!(window.evidence.len() <= HEAD + NOTABLE + TAIL);
        assert_eq!(window.omitted + window.evidence.len() as u64, 200);
        assert!(
            window
                .evidence
                .iter()
                .any(|line| line.text == "ERROR retained")
        );
        assert!(
            window
                .evidence
                .iter()
                .all(|line| line.text != "duplicate" && line.text != "old")
        );
    }
}
