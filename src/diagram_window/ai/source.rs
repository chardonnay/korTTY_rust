//! A bounded, line-numbered view of the original editor source.

use std::fmt::Write as _;

const CONDENSE_AFTER_LINES: usize = 400;
const MAX_OUTLINE_LINES: usize = 500;
const MAX_CONTEXT_BYTES: usize = 64 * 1024;
const MIN_STRUCTURE_LINES: usize = 24;
const WIDENED_INDENT: usize = 4;

pub(super) struct Source<'a> {
    lines: Vec<&'a str>,
    visible: Vec<usize>,
    pub(super) text: String,
    pub(super) condensed: bool,
}

impl<'a> Source<'a> {
    pub(super) fn new(script: &'a str) -> Result<Self, String> {
        let lines: Vec<_> = script
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
            .collect();
        let width = digits(lines.len());
        let full_size = lines
            .iter()
            .map(|line| line.len() + width + 4)
            .sum::<usize>();
        let condensed = lines.len() > CONDENSE_AFTER_LINES || full_size > MAX_CONTEXT_BYTES;
        let visible = if condensed {
            select(&lines, width)
        } else {
            (0..lines.len()).collect()
        };
        if visible.is_empty() {
            return Err("No complete source lines fit into the AI diagram context".into());
        }
        let mut text = String::new();
        let mut cursor = 0;
        for &index in &visible {
            omit(&mut text, index - cursor);
            let _ = writeln!(text, "{:>width$} | {}", index + 1, lines[index]);
            cursor = index + 1;
        }
        omit(&mut text, lines.len() - cursor);
        Ok(Self {
            lines,
            visible,
            text,
            condensed,
        })
    }

    pub(super) fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub(super) fn visible_line(&self, one_based: u32) -> Option<&str> {
        let index = (one_based as usize).checked_sub(1)?;
        self.visible.binary_search(&index).ok()?;
        self.lines.get(index).copied()
    }
}

fn omit(text: &mut String, count: usize) {
    if count == 1 {
        text.push_str("… 1 line omitted …\n");
    } else if count > 1 {
        let _ = writeln!(text, "… {count} lines omitted …");
    }
}

fn digits(value: usize) -> usize {
    value.max(1).ilog10() as usize + 1
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
    Boundary,
    Definition,
    TopLevel,
    Nested,
}

struct Candidate {
    index: usize,
    indent: usize,
    rank: Rank,
    control: bool,
    cost: usize,
}

fn select(lines: &[&str], width: usize) -> Vec<usize> {
    // A selected line can introduce one omission marker; reserve space for the
    // trailing marker as well. The estimate bounds the rendered UTF-8 body.
    let marker = width + "…  lines omitted …\n".len();
    let budget = MAX_CONTEXT_BYTES - marker;
    let mut candidates: Vec<_> = lines
        .iter()
        .enumerate()
        .filter_map(|(index, &line)| {
            let text = line.trim_start();
            let cost = line.len() + width + 4 + marker;
            if text.is_empty() || cost > budget || (index > 0 && is_comment(text)) {
                return None;
            }
            let indent = line[..line.len() - text.len()]
                .chars()
                .map(|ch| if ch == '\t' { 4 } else { 1 })
                .sum();
            let rank = if index == 0 && text.starts_with("#!") {
                Rank::Boundary
            } else if definition(text) {
                Rank::Definition
            } else if indent == 0 {
                Rank::TopLevel
            } else {
                Rank::Nested
            };
            Some(Candidate {
                index,
                indent,
                rank,
                control: control_flow(text),
                cost,
            })
        })
        .collect();
    if let Some(last) = candidates
        .iter_mut()
        .rfind(|candidate| candidate.rank == Rank::TopLevel)
    {
        last.rank = Rank::Boundary;
    }
    let relevant = |candidate: &Candidate, allowance| {
        candidate.rank != Rank::Nested
            || candidate.indent <= allowance
            || (candidate.indent <= WIDENED_INDENT && candidate.control)
    };
    let mut allowance = 0;
    if candidates
        .iter()
        .filter(|candidate| relevant(candidate, allowance))
        .count()
        < MIN_STRUCTURE_LINES
    {
        allowance = WIDENED_INDENT;
    }
    if candidates
        .iter()
        .all(|candidate| !relevant(candidate, allowance))
    {
        allowance = candidates
            .iter()
            .map(|candidate| candidate.indent)
            .min()
            .unwrap_or(0);
    }
    let mut chosen = Vec::new();
    for rank in [
        Rank::Boundary,
        Rank::Definition,
        Rank::TopLevel,
        Rank::Nested,
    ] {
        let group: Vec<_> = candidates
            .iter()
            .filter(|candidate| candidate.rank == rank && relevant(candidate, allowance))
            .collect();
        let room = MAX_OUTLINE_LINES - chosen.len();
        if group.len() <= room {
            chosen.extend(group);
        } else if room > 0 && (rank != Rank::Nested || chosen.is_empty()) {
            chosen.extend((0..room).map(|position| {
                let index = if room == 1 {
                    group.len() - 1
                } else {
                    position * (group.len() - 1) / (room - 1)
                };
                group[index]
            }));
        }
    }
    let mut total = chosen.iter().map(|candidate| candidate.cost).sum::<usize>();
    if total > budget {
        // Preserve the structural priorities while dropping expensive lines
        // from each tier. Visible lines always remain verbatim.
        chosen.sort_unstable_by_key(|candidate| (candidate.rank, candidate.cost));
        while total > budget {
            total -= chosen.pop().expect("nonzero context cost").cost;
        }
    }
    let mut visible: Vec<_> = chosen
        .into_iter()
        .map(|candidate| candidate.index)
        .collect();
    visible.sort_unstable();
    visible
}

fn is_comment(text: &str) -> bool {
    ["#", "//", "--", ";", "::", "\"\"\"", "'''", "<#", "/*", "*"]
        .into_iter()
        .any(|prefix| text.starts_with(prefix))
}

fn first_word(text: &str) -> &str {
    text.split(|ch: char| ch != '_' && !ch.is_alphanumeric())
        .next()
        .unwrap_or_default()
}

fn definition(text: &str) -> bool {
    let keyword = first_word(text);
    let tail = text[keyword.len()..].trim_start();
    if keyword == "async" && text[keyword.len()..].starts_with(char::is_whitespace) {
        return definition_keyword(first_word(tail));
    }
    if definition_keyword(keyword) {
        return true;
    }
    let name_end = text
        .bytes()
        .position(|byte| !byte.is_ascii_alphanumeric() && !b"_.:-".contains(&byte))
        .unwrap_or(text.len());
    text.bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && text[name_end..]
            .trim_start()
            .strip_prefix('(')
            .is_some_and(|rest| rest.trim_start().starts_with(')'))
}

fn definition_keyword(word: &str) -> bool {
    matches!(
        word,
        "function" | "def" | "sub" | "proc" | "class" | "interface" | "module" | "task" | "rule"
    )
}

fn control_flow(text: &str) -> bool {
    let text = text.strip_prefix('}').unwrap_or(text).trim_start();
    matches!(
        first_word(text),
        "if" | "elif"
            | "elsif"
            | "else"
            | "fi"
            | "case"
            | "esac"
            | "switch"
            | "when"
            | "while"
            | "until"
            | "for"
            | "foreach"
            | "do"
            | "done"
            | "try"
            | "catch"
            | "except"
            | "finally"
            | "return"
            | "exit"
            | "die"
            | "break"
            | "continue"
            | "trap"
            | "end"
            | "main"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outline_bounds_bytes_and_lines_without_shortening_utf8_quotes() {
        let line = format!("echo {}", "ä🦀".repeat(90));
        let script = format!("{line}\n").repeat(1_000);
        let source = Source::new(&script).unwrap();
        assert!(source.condensed);
        assert!(source.text.len() <= MAX_CONTEXT_BYTES);
        assert!(source.visible.len() <= MAX_OUTLINE_LINES);
        assert_eq!(source.visible_line(1_000), Some(line.as_str()));
        for index in source.visible {
            assert_eq!(source.lines[index], line);
        }

        let oversized = "ä".repeat(MAX_CONTEXT_BYTES);
        assert!(Source::new(&oversized).is_err());
    }

    #[test]
    fn even_sampling_preserves_the_interpreter_and_final_entry_point() {
        let mut script = "#!/bin/sh\n".to_owned();
        for index in 1..=1_000 {
            writeln!(script, "fn_{index}() {{ echo ready; }}").unwrap();
        }
        script.push_str("fn_1000");
        let source = Source::new(&script).unwrap();
        assert_eq!(source.visible.len(), MAX_OUTLINE_LINES);
        assert_eq!(source.visible_line(1), Some("#!/bin/sh"));
        assert_eq!(source.visible_line(2), Some("fn_1() { echo ready; }"));
        assert_eq!(source.visible_line(1001), Some("fn_1000() { echo ready; }"));
        assert_eq!(source.visible_line(1002), Some("fn_1000"));
    }
}
