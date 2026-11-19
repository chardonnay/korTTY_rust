use std::io::{self, Read};
use std::path::Path;

use crate::{clipboard_policy::ClipboardPolicy, profile};

use super::{mask_ai_prompt, saved_chats};

const CAPACITY: usize = 10;
pub(crate) const MAX_QUESTION_CHARS: usize = 1_000;
pub(crate) const MANUAL_FILE: &str = "manual-questions.json";
pub(crate) const TERMINAL_FILE: &str = "terminal-questions.json";
pub(crate) const WORKFLOW_FILE: &str = "workflow-instructions.json";
pub(super) const MAX_WORKFLOW_INSTRUCTIONS: usize = 4096;

#[derive(Clone, Copy, Default)]
pub(crate) enum Kind {
    #[default]
    Manual,
    Terminal,
    Workflow,
}

impl Kind {
    fn file(self) -> &'static str {
        match self {
            Self::Manual => MANUAL_FILE,
            Self::Terminal => TERMINAL_FILE,
            Self::Workflow => WORKFLOW_FILE,
        }
    }

    pub(crate) fn max_bytes(self) -> usize {
        match self {
            Self::Manual => 64 * 1024,
            Self::Terminal => 192 * 1024,
            Self::Workflow => 96 * 1024,
        }
    }

    fn valid(self, question: &str) -> bool {
        if question.trim().is_empty() {
            return false;
        }
        match self {
            Self::Manual => {
                question.chars().count() <= MAX_QUESTION_CHARS
                    && !question.chars().any(char::is_control)
            }
            Self::Terminal => {
                question.len() <= super::terminal_ai::MAX_QUESTION_BYTES
                    && !question.chars().any(|character| {
                        character.is_control() && !matches!(character, '\n' | '\t')
                    })
            }
            Self::Workflow => {
                question.len() <= MAX_WORKFLOW_INSTRUCTIONS
                    && question
                        .chars()
                        .all(crate::session_journal::is_display_character)
            }
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct History {
    kind: Kind,
    entries: Vec<String>,
}

impl History {
    pub(super) fn load() -> io::Result<Self> {
        Self::load_at(&profile::root()?, Kind::Manual)
    }

    pub(super) fn load_at(root: &Path, kind: Kind) -> io::Result<Self> {
        Self::read(&root.join(kind.file()), kind)
    }

    fn read(path: &Path, kind: Kind) -> io::Result<Self> {
        let file = match profile::open_preference(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Self {
                    kind,
                    entries: Vec::new(),
                });
            }
            Err(error) => return Err(error),
        };
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "question history must be a regular file",
            ));
        }
        let mut bytes = Vec::new();
        file.take(kind.max_bytes() as u64 + 1)
            .read_to_end(&mut bytes)?;
        decode(&bytes, kind)
    }

    pub(super) fn entries(&self) -> &[String] {
        &self.entries
    }

    pub(super) fn record(&mut self, question: String) -> bool {
        if !self.kind.valid(&question) || self.entries.first() == Some(&question) {
            return false;
        }
        self.entries.retain(|entry| *entry != question);
        self.entries.insert(0, question);
        self.entries.truncate(CAPACITY);
        true
    }

    pub(super) fn save(&self) -> io::Result<()> {
        self.save_at(&profile::root()?)
    }

    pub(super) fn save_at(&self, root: &Path) -> io::Result<()> {
        self.write(&root.join(self.kind.file()))
    }

    pub(super) fn clear(&mut self) -> bool {
        let changed = !self.entries.is_empty();
        self.entries.clear();
        changed
    }

    pub(super) fn update_at(
        &mut self,
        root: &Path,
        update: impl FnOnce(&mut Self) -> bool,
    ) -> io::Result<()> {
        let mut next = self.clone();
        if update(&mut next) {
            next.save_at(root)?;
            *self = next;
        }
        Ok(())
    }

    fn write(&self, path: &Path) -> io::Result<()> {
        profile::save_atomic(path, &serde_json::to_vec(&self.entries)?)
    }
}

fn decode(bytes: &[u8], kind: Kind) -> io::Result<History> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid question history");
    if bytes.len() > kind.max_bytes() {
        return Err(invalid());
    }
    let entries: Vec<String> = serde_json::from_slice(bytes)?;
    if entries.len() > CAPACITY
        || entries
            .iter()
            .enumerate()
            .any(|(index, entry)| !kind.valid(entry) || entries[..index].contains(entry))
    {
        return Err(invalid());
    }
    Ok(History { kind, entries })
}

pub(crate) fn validate(bytes: &[u8], kind: Kind) -> io::Result<()> {
    decode(bytes, kind).map(|_| ())
}

pub(super) fn read_java_history(
    wrapper: roxmltree::Node<'_, '_>,
    kind: Kind,
    omitted: &mut usize,
) -> io::Result<Option<(Vec<u8>, usize)>> {
    let redactor = saved_chats::java_xml_redactor(&ClipboardPolicy::load().terminal_logging)?;
    let tag = match kind {
        Kind::Manual => "question",
        Kind::Terminal => "prompt",
        Kind::Workflow => "entry",
    };
    let mut entries = Vec::new();
    for node in wrapper.children().filter(roxmltree::Node::is_element) {
        let question = (entries.len() < CAPACITY
            && node.has_tag_name(tag)
            && node.tag_name().namespace().is_none()
            && node.attributes().next().is_none()
            && node.children().all(|child| child.is_text()))
        .then(|| node.text().map(str::trim))
        .flatten()
        .filter(|question| kind.valid(question))
        .and_then(|question| mask_ai_prompt(&redactor, question).ok())
        .filter(|question| kind.valid(question) && !entries.contains(question));
        if let Some(question) = question {
            entries.push(question);
        } else {
            *omitted += 1;
        }
    }
    if entries.is_empty() {
        return Ok(None);
    }
    Ok(Some((serde_json::to_vec(&entries)?, entries.len())))
}

pub(crate) fn merge_java_history(
    local: Option<&[u8]>,
    incoming: &[u8],
    kind: Kind,
) -> io::Result<Option<Vec<u8>>> {
    let mut local = local
        .map(|bytes| decode(bytes, kind))
        .transpose()?
        .unwrap_or_default();
    let original_count = local.entries.len();
    for question in decode(incoming, kind)?.entries {
        if local.entries.len() == CAPACITY {
            break;
        }
        if !local.entries.contains(&question) {
            local.entries.push(question);
        }
    }
    if local.entries.len() == original_count {
        return Ok(None);
    }
    Ok(Some(serde_json::to_vec(&local.entries)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_question_history_is_bounded_and_preserves_order_after_reopening() {
        let scratch = crate::snippets::tests::Profile::new();
        let path = scratch.0.join("manual-questions.json");
        let mut history = History::read(&path, Kind::Manual).unwrap();
        assert!(history.entries().is_empty());
        for index in 1..=12 {
            assert!(history.record(format!("Question {index}")));
        }
        assert!(history.record("Question 4".into()));
        assert!(!history.record("Question 4".into()));
        assert!(!history.record("a\0b".into()));
        assert!(!history.record("界".repeat(MAX_QUESTION_CHARS + 1)));
        history.write(&path).unwrap();
        let reopened = History::read(&path, Kind::Manual).unwrap();
        assert_eq!(reopened.entries(), history.entries());
        assert_eq!(reopened.entries().len(), 10);
        assert_eq!(reopened.entries()[..2], ["Question 4", "Question 12"]);
        assert_eq!(reopened.entries().last().unwrap(), "Question 3");
    }
}
