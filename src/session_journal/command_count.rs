//! Counts retained shell command blocks for the full journal header.

use std::collections::{HashSet, VecDeque};
use std::io;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};

use jiff::Timestamp;
use regex::Regex;
use zeroize::{Zeroize, Zeroizing};

use super::summary_plan::{COMMAND_FINISHED, SensitiveBlocks, filters};
use super::{CaptureKind, capture, child, invalid, number, viewer::Source};

const LOOKAHEAD: usize = 3;
const LOOKBACK: usize = 6;
const PROMPT_UNITS: usize = 160;
const MAX_LEARNED_PROMPTS: usize = 4_096;
const HELP_LINES: usize = 40;
const PROMPT_ENDINGS: &str = "$#%>❯➜»λ";

static USER_PROMPT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"^\s*(?:\([^)]{1,40}\)\s*)?(?:\[?[a-z_][a-z0-9_.-]{0,31}@[A-Za-z0-9_.-]+",
        r"(?:[: ][^\s\]$#%]*)?\]?\s?[$#%]|(?:ba|z|k|da)?sh-[0-9.]+#)(?:\s.*)?$",
    ))
    .expect("static user shell prompt")
});
static ECHO_START: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[$#%]\s+").expect("static shell echo separator"));
static REPEAT_SUFFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s\(×[0-9]+\)$").expect("static repeated shell prompt suffix"));

/// Reclassifies only the retained records. Missing or unreadable capture cannot
/// replace an imported count, while a clean unclosed final part can be recovered.
pub(super) fn read(
    source: &Source,
    cancelled: &AtomicBool,
) -> io::Result<(Option<Timestamp>, Option<u64>)> {
    let mut latest: Option<Timestamp> = None;
    let mut counter = Counter::default();
    let mut budget = capture::MAX_CAPTURE_BYTES;
    let status = source.with_document(|opened, document| {
        let meta = child(document.root_element(), "meta")
            .ok_or_else(|| invalid("Missing session journal metadata"))?;
        let parts = number::<u32>(meta, "logParts")?.unwrap_or(1);
        capture::retrieval::visit(
            opened,
            source.id(),
            parts,
            None,
            &mut budget,
            &|| cancelled.load(Ordering::Acquire),
            |row| {
                let record = row.record;
                latest = Some(
                    latest.map_or(record.timestamp, |previous| previous.max(record.timestamp)),
                );
                counter.push(record.kind, &record.text, record.partial, record.redacted);
                true
            },
        )
    })?;
    // Torn records have no retained entry. A missing part, in contrast, can
    // hide already accepted records and must not overwrite an existing count.
    let commands = (status.missing_parts == 0 && status.unreadable_parts == 0 && !status.limited)
        .then(|| counter.finish());
    Ok((latest, commands))
}

pub(super) struct Counter {
    pending: VecDeque<Record>,
    last_output: Option<Record>,
    prompts: Prompts,
    prefix: Option<BlockContent>,
    block: Option<Block>,
    shell_finished: bool,
    saw_output: bool,
    count: u64,
    sensitive: SensitiveBlocks,
}

impl Default for Counter {
    fn default() -> Self {
        Self {
            pending: VecDeque::new(),
            last_output: None,
            prompts: Prompts::default(),
            prefix: Some(BlockContent::default()),
            block: None,
            shell_finished: false,
            saw_output: false,
            count: 0,
            sensitive: SensitiveBlocks::default(),
        }
    }
}

#[derive(Default)]
struct Prompts(HashSet<String>);

impl Prompts {
    fn can_learn(&self, prompt: &str) -> bool {
        // Prompts may include a changing directory or arbitrary terminal text.
        // Keep learned state bounded independently of retained capture size.
        self.0.len() < MAX_LEARNED_PROMPTS
            && !prompt.is_empty()
            && prompt.encode_utf16().count() <= PROMPT_UNITS
            && !self.0.contains(prompt)
    }

    fn remembers(&self, prompt: &str) -> bool {
        self.0.contains(prompt)
            || prompt
                .match_indices(' ')
                .take_while(|(offset, _)| *offset <= PROMPT_UNITS * 3)
                .any(|(offset, _)| self.0.contains(&prompt[..offset]))
    }
}

impl Drop for Prompts {
    fn drop(&mut self) {
        for mut prompt in self.0.drain() {
            prompt.zeroize();
        }
    }
}

struct Record {
    kind: CaptureKind,
    text: Zeroizing<String>,
    partial: bool,
    redacted: bool,
}

impl Record {
    fn output(&self) -> bool {
        matches!(self.kind, CaptureKind::Output | CaptureKind::Seed)
    }
}

impl Counter {
    pub(super) fn push(&mut self, kind: CaptureKind, text: &str, partial: bool, redacted: bool) {
        let private = matches!(
            kind,
            CaptureKind::Input | CaptureKind::Output | CaptureKind::Seed
        ) && self.sensitive.excludes(text);
        self.pending.push_back(Record {
            kind,
            text: Zeroizing::new(
                if private || redacted && kind == CaptureKind::Input {
                    ""
                } else {
                    text
                }
                .into(),
            ),
            partial,
            redacted: redacted || private,
        });
        if self.pending.len() > LOOKAHEAD {
            self.process();
        }
    }

    pub(super) fn finish(mut self) -> u64 {
        while !self.pending.is_empty() {
            self.process();
        }
        self.close_block();
        self.count
    }

    fn process(&mut self) {
        let record = self.pending.pop_front().expect("pending capture record");
        if record.kind == CaptureKind::Note && completion(&record.text).is_some() {
            self.shell_finished = true;
        }
        if record.kind == CaptureKind::Input && !record.redacted && !record.text.trim().is_empty() {
            let prompt = self.prompt_for(&record.text);
            if prompt.is_some() || self.shell_finished || !self.saw_output {
                if let Some(prompt) = prompt.or_else(|| {
                    self.last_output
                        .as_ref()
                        .map(|previous| without_typed(&previous.text, &record.text).trim())
                }) && self.prompts.can_learn(prompt)
                {
                    let prompt = prompt.to_owned();
                    self.prompts.0.insert(prompt);
                }
                self.close_block();
                self.block = Some(Block {
                    command: Zeroizing::new(record.text.trim().to_owned()),
                    content: self.prefix.take().unwrap_or_default(),
                });
                self.shell_finished = false;
            }
        }
        if let Some(block) = &mut self.block {
            block.content.observe(&record);
        } else if record.kind == CaptureKind::Note
            || record.output() && (record.partial || record.text.trim().is_empty())
        {
            if let Some(prefix) = &mut self.prefix {
                prefix.observe(&record);
            }
        } else {
            self.prefix = None;
        }
        if record.output() {
            self.saw_output = true;
            self.last_output = Some(record);
        }
    }

    fn prompt_for(&self, typed: &str) -> Option<&str> {
        self.last_output
            .as_ref()
            .and_then(|record| self.prompt_of(record, typed))
            .or_else(|| {
                self.pending
                    .iter()
                    .take_while(|record| record.kind != CaptureKind::Input)
                    .find(|record| record.kind == CaptureKind::Output && !record.partial)
                    .and_then(|record| self.prompt_of(record, typed))
            })
    }

    fn prompt_of<'a>(&self, record: &'a Record, typed: &str) -> Option<&'a str> {
        let text = record.text.trim_end();
        let rest = without_typed(text, typed);
        let prompt = rest.trim();
        if prompt.is_empty() {
            return None;
        }
        let typed_at = record.partial || rest.len() < text.len();
        (idle(prompt)
            || USER_PROMPT.is_match(prompt)
            || self.prompts.remembers(prompt)
            || typed_at
                && prompt.encode_utf16().count() <= PROMPT_UNITS
                && ends_like_prompt(prompt))
        .then_some(prompt)
    }

    fn close_block(&mut self) {
        if self.block.take().is_some_and(Block::counts) {
            self.count += 1;
        }
    }
}

struct Block {
    command: Zeroizing<String>,
    content: BlockContent,
}

impl Block {
    fn counts(self) -> bool {
        let mut content = self
            .content
            .with_last_partial
            .unwrap_or(self.content.retained);
        while let Some(retained) = content.recent.pop_front() {
            content.retain_help(retained);
        }
        content.retained_inputs > 0 && !filters::help_only(&self.command, content.help_heading)
    }
}

#[derive(Default)]
struct BlockContent {
    retained: ContentState,
    // The final partial output survives at its original position. Until a
    // subsequent output arrives, both interpretations can still be final.
    with_last_partial: Option<ContentState>,
}

impl BlockContent {
    fn observe(&mut self, record: &Record) {
        if record.output() {
            self.with_last_partial = if record.partial {
                let mut state = self.retained.clone();
                state.observe(record);
                Some(state)
            } else {
                self.retained.observe(record);
                None
            };
        } else {
            self.retained.observe(record);
            if let Some(state) = &mut self.with_last_partial {
                state.observe(record);
            }
        }
    }
}

#[derive(Clone, Default)]
struct ContentState {
    recent: VecDeque<Content>,
    retained_inputs: usize,
    help_lines: usize,
    help_heading: bool,
    suggestions: bool,
}

#[derive(Clone)]
enum Content {
    Input {
        name: Option<Zeroizing<String>>,
        removed: bool,
    },
    Output {
        echo: Option<Zeroizing<String>>,
        removed: bool,
        help: Option<bool>,
    },
    Other(Option<bool>),
}

impl ContentState {
    fn observe(&mut self, record: &Record) {
        match record.kind {
            CaptureKind::Input => {
                self.suggestions = false;
                self.retained_inputs += 1;
                self.remember(Content::Input {
                    name: (!record.redacted)
                        .then(|| command_name(&record.text))
                        .flatten(),
                    removed: false,
                });
            }
            CaptureKind::Output | CaptureKind::Seed => self.output(&record.text),
            CaptureKind::Note if completion(&record.text) == Some(true) => self.other(&record.text),
            CaptureKind::Screenshot => self.other(&record.text),
            CaptureKind::Note => {}
        }
    }

    fn other(&mut self, text: &str) {
        self.suggestions = false;
        self.remember(Content::Other(help_candidate(text)));
    }

    fn output(&mut self, text: &str) {
        let user_prompt = USER_PROMPT.is_match(text);
        let prompt = user_prompt || idle(text);
        let not_found = filters::not_found_command(text);
        if prompt || not_found.is_some() {
            self.suggestions = false;
        }
        let removed = self.suggestions || not_found.is_some_and(|name| self.remove_attempt(name));
        self.suggestions = removed;
        let echo = if user_prompt {
            ECHO_START
                .find(text)
                .and_then(|boundary| command_name(&text[boundary.end()..]))
        } else {
            None
        };
        self.remember(Content::Output {
            echo,
            removed,
            help: help_candidate(text),
        });
    }

    fn remove_attempt(&mut self, name: &str) -> bool {
        let mut input = false;
        let mut echo = false;
        for record in self.recent.iter_mut().rev() {
            match record {
                Content::Input {
                    name: Some(candidate),
                    removed,
                } if !input && !*removed && candidate.as_str() == name => {
                    *removed = true;
                    self.retained_inputs -= 1;
                    input = true;
                }
                Content::Output {
                    echo: Some(candidate),
                    removed,
                    ..
                } if !echo && !*removed && candidate.as_str() == name => {
                    *removed = true;
                    echo = true;
                }
                _ => {}
            }
            if input && echo {
                break;
            }
        }
        input || echo
    }

    fn remember(&mut self, content: Content) {
        if self.recent.len() == LOOKBACK
            && let Some(retained) = self.recent.pop_front()
        {
            self.retain_help(retained);
        }
        self.recent.push_back(content);
    }

    fn retain_help(&mut self, content: Content) {
        let hint = match content {
            Content::Output {
                removed: false,
                help,
                ..
            }
            | Content::Other(help) => help,
            _ => None,
        };
        if let Some(heading) = hint
            && self.help_lines < HELP_LINES
        {
            self.help_lines += 1;
            self.help_heading |= heading;
        }
    }
}

fn help_candidate(text: &str) -> Option<bool> {
    (!text.trim().is_empty()).then(|| filters::help_heading(text))
}

fn idle(text: &str) -> bool {
    filters::idle(text)
        || REPEAT_SUFFIX
            .find(text)
            .is_some_and(|suffix| filters::idle(&text[..suffix.start()]))
}

fn command_name(line: &str) -> Option<Zeroizing<String>> {
    let mut words = line.split_whitespace().peekable();
    while let Some(word) = words.next() {
        if words.peek().is_none() || word != "sudo" && !word.starts_with('-') && !word.contains('=')
        {
            return Some(Zeroizing::new(word.into()));
        }
    }
    None
}

fn without_typed<'a>(line: &'a str, typed: &str) -> &'a str {
    let line = line.trim_end();
    let typed = typed.trim();
    if let Some(rest) = line.strip_suffix(typed) {
        return rest;
    }
    let length = line.len().min(typed.len());
    if length == 0 {
        return line;
    }
    // Track the longest prefix of the command that ends at each output byte.
    // The failure links keep long partially echoed commands linear in size.
    let pattern = &typed.as_bytes()[..length];
    let mut fallback = vec![0; pattern.len()];
    let mut matched = 0;
    for (index, &byte) in pattern.iter().enumerate().skip(1) {
        matched = advance_prefix(pattern, &fallback, matched, byte);
        fallback[index] = matched;
    }
    matched = 0;
    for &byte in &line.as_bytes()[line.len() - length..] {
        matched = advance_prefix(pattern, &fallback, matched, byte);
    }
    // Both strings are UTF-8. A byte match ending at the end of the output
    // necessarily starts and ends on character boundaries in both strings.
    &line[..line.len() - matched]
}

fn advance_prefix(pattern: &[u8], fallback: &[usize], mut matched: usize, byte: u8) -> usize {
    while matched > 0 && pattern.get(matched) != Some(&byte) {
        matched = fallback[matched - 1];
    }
    matched + usize::from(pattern.get(matched) == Some(&byte))
}

fn completion(text: &str) -> Option<bool> {
    let tail = text.strip_prefix(COMMAND_FINISHED)?;
    if tail.is_empty() {
        return Some(false);
    }
    let status = tail.strip_prefix(" (exit ")?.strip_suffix(')')?;
    let digits = status.strip_prefix('-').unwrap_or(status);
    (matches!(digits.len(), 1..=9) && digits.bytes().all(|byte| byte.is_ascii_digit()))
        .then_some(true)
}

fn ends_like_prompt(text: &str) -> bool {
    let mut characters = text.chars().rev();
    characters.next().is_some_and(|last| {
        PROMPT_ENDINGS.contains(last)
            && characters
                .next()
                .is_none_or(|previous| previous != last && !previous.is_numeric())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use CaptureKind::{Input, Note, Output};

    #[test]
    fn command_blocks_drop_help_failed_starts_and_separate_program_replies() {
        let mut counter = Counter::default();
        for (kind, text, partial, redacted) in [
            (Output, "Welcome to the host", false, false),
            (Input, "standalone program reply", false, false),
            (Output, "Processing standalone reply", false, false),
            (Output, "daniel@fedora:~$", false, false),
            (Input, "sudo id", false, false),
            (Output, "[sudo] password for daniel:", false, false),
            (Input, "masked response", false, true),
            (Output, "uid=0(root)", false, false),
            (Note, COMMAND_FINISHED, false, false),
            (Input, "ls --help", false, false),
            (Note, COMMAND_FINISHED, false, false),
            (Input, "tool -h", false, false),
            (Output, "Usage: tool [options]", true, false),
            (
                Note,
                "shell integration: command finished (exit 0)",
                false,
                false,
            ),
            (Input, "df -h", false, false),
            (Output, "Usage: superseded partial", true, false),
            (
                Output,
                "Filesystem Size Used Avail Use% Mounted on",
                false,
                false,
            ),
            (Note, COMMAND_FINISHED, false, false),
            (Input, "sudo -E VAR=1 mistsart", false, false),
            (
                Output,
                "daniel@fedora:~$ sudo -E VAR=1 mistsart",
                false,
                false,
            ),
            (Output, "sudo: mistsart: command not found", false, false),
            (Output, "Similar command: start", false, false),
            (
                Note,
                "shell integration: command finished (exit 127)",
                false,
                false,
            ),
            (Input, "cat missing", false, false),
            (
                Output,
                "cat: missing: No such file or directory",
                false,
                false,
            ),
        ] {
            counter.push(kind, text, partial, redacted);
        }
        assert_eq!(counter.finish(), 3);
    }

    #[test]
    fn lookahead_uses_physical_rows_and_remembers_a_prompt() {
        let mut counter = Counter::default();
        for (kind, text) in [
            (Output, "Welcome"),
            (Input, "first"),
            (Note, "non-content marker"),
            (CaptureKind::Screenshot, ""),
            (Output, "test> first"),
            (Output, "test>"),
            (Input, "second"),
            (Output, "working"),
            (Input, "a program reply"),
            (Input, "another program reply"),
            (Output, "test> another program reply"),
        ] {
            counter.push(kind, text, false, false);
        }
        assert_eq!(counter.finish(), 3);
    }

    #[test]
    fn learned_prompt_memory_stays_bounded_across_the_capture() {
        let mut counter = Counter::default();
        for index in 0..=MAX_LEARNED_PROMPTS {
            counter.push(Output, &format!("custom-{index}-path>"), true, false);
            counter.push(Input, "command", false, false);
        }
        for _ in 0..LOOKAHEAD {
            counter.push(Note, "non-content marker", false, false);
        }
        assert_eq!(counter.prompts.0.len(), MAX_LEARNED_PROMPTS);
        assert!(counter.prompts.remembers("custom-0-path> terminal output"));
        assert!(
            !counter
                .prompts
                .remembers(&format!("custom-{MAX_LEARNED_PROMPTS}-path>"))
        );
        assert_eq!(counter.finish(), MAX_LEARNED_PROMPTS as u64 + 1);
    }

    #[test]
    fn the_last_partial_error_keeps_its_position_before_exit_notes() {
        let mut counter = Counter::default();
        counter.push(Input, "missing", false, false);
        counter.push(Output, "bash: missing: command not found", true, false);
        for _ in 0..LOOKBACK {
            counter.push(
                Note,
                "shell integration: command finished (exit 127)",
                false,
                false,
            );
        }
        assert_eq!(counter.finish(), 0);
    }

    #[test]
    fn partial_echo_matches_only_complete_utf8_at_the_end() {
        assert_eq!(without_typed("prompt> prë", "prëfix"), "prompt> ");
        assert_eq!(without_typed("prompt> baabaab", "baabaabb"), "prompt> ");
        assert_eq!(without_typed("海", "海上"), "");
        assert_eq!(without_typed("ends elsewhere", "a"), "ends elsewhere");
    }
}
