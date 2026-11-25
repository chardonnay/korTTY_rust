//! General controls sharing the existing snippet and prompt-history stores.

use std::{io, path::Path};

use crate::snippet_window::ai_assist::{ALTERNATIVE_COUNTS, DEFAULT_ALTERNATIVE_COUNT};
use crate::terminal_agent::history::{self, Snapshot};

use super::tab_titles::WindowSettings;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SnippetValues {
    pub(crate) instructions: bool,
    pub(crate) alternatives: u8,
}

impl From<&WindowSettings> for SnippetValues {
    fn from(settings: &WindowSettings) -> Self {
        Self {
            instructions: settings.snippet_ai_additional_instructions(),
            alternatives: settings
                .snippet_alternative_count
                .unwrap_or(DEFAULT_ALTERNATIVE_COUNT),
        }
    }
}

#[derive(Clone)]
pub(crate) struct State {
    snippets: Option<SnippetValues>,
    saved_snippets: Option<SnippetValues>,
    history: Option<Snapshot>,
    pub(crate) history_limit: String,
    pub(crate) problem: String,
    load_problem: String,
    snippet_update: Option<SnippetValues>,
    history_update: Option<Snapshot>,
}

impl State {
    pub(crate) fn load(root: &Path) -> Self {
        let mut problems = Vec::new();
        let snippets = match read_settings(root) {
            Ok(settings) => Some(SnippetValues::from(&settings)),
            Err(error) => {
                problems.push(format!("Could not load snippet defaults ({error})."));
                None
            }
        };
        let history = match history::list(root, false) {
            Ok(snapshot) => Some(snapshot),
            Err(error) => {
                problems.push(format!("Could not load the prompt limit ({error})."));
                None
            }
        };
        Self {
            snippets,
            saved_snippets: snippets,
            history_limit: history
                .as_ref()
                .map_or_else(String::new, |h| h.limit.to_string()),
            history,
            problem: problems.join(" "),
            load_problem: problems.join(" "),
            snippet_update: None,
            history_update: None,
        }
    }

    pub(crate) fn snippets(&self) -> Option<SnippetValues> {
        self.snippets
    }
    pub(crate) fn history_available(&self) -> bool {
        self.history.is_some()
    }
    pub(crate) fn set_instructions(&mut self, value: bool) {
        if let Some(snippets) = &mut self.snippets {
            snippets.instructions = value;
        }
    }
    pub(crate) fn set_alternatives(&mut self, value: u8) {
        if ALTERNATIVE_COUNTS.contains(&value)
            && let Some(snippets) = &mut self.snippets
        {
            snippets.alternatives = value;
        }
    }
    pub(crate) fn set_history_limit(&mut self, mut value: String) {
        if self.history_available() {
            value.truncate(value.floor_char_boundary(4));
            self.history_limit = value;
        }
    }
    fn parsed_limit(&self) -> Option<usize> {
        self.history_limit
            .parse()
            .ok()
            .filter(|v| (history::MIN_LIMIT..=history::MAX_LIMIT).contains(v))
    }
    pub(crate) fn valid(&self) -> bool {
        !self.history_available() || self.parsed_limit().is_some()
    }
    pub(crate) fn dirty(&self) -> bool {
        self.snippets != self.saved_snippets
            || self
                .history
                .as_ref()
                .is_some_and(|h| self.parsed_limit() != Some(h.limit))
    }

    /// Rebase only untouched controls when another native settings surface saves.
    pub(crate) fn install_snippets(&mut self, current: SnippetValues) {
        let (Some(draft), Some(before)) = (&mut self.snippets, &mut self.saved_snippets) else {
            return;
        };
        if draft.instructions == before.instructions {
            draft.instructions = current.instructions;
            before.instructions = current.instructions;
        }
        if draft.alternatives == before.alternatives {
            draft.alternatives = current.alternatives;
            before.alternatives = current.alternatives;
        }
    }

    /// Called after the AI configuration commits. Each existing store keeps its
    /// own atomic write and conflict checks; a partial failure remains a draft.
    pub(crate) fn save(&mut self, root: &Path) -> Option<String> {
        let mut errors = Vec::new();
        if self.snippets != self.saved_snippets
            && let (Some(before), Some(draft)) = (self.saved_snippets, self.snippets)
        {
            let result = (|| {
                let mut current = read_settings(root)?;
                let values = SnippetValues::from(&current);
                if (draft.instructions != before.instructions
                    && values.instructions != before.instructions)
                    || (draft.alternatives != before.alternatives
                        && values.alternatives != before.alternatives)
                {
                    return Err(io::Error::other(
                        "Snippet defaults changed in another dialog. Reopen AI Manager to review them",
                    ));
                }
                if draft.instructions != before.instructions {
                    current.snippet_ai_additional_instructions = Some(draft.instructions);
                }
                if draft.alternatives != before.alternatives {
                    current.snippet_alternative_count = Some(draft.alternatives);
                }
                current.save(&crate::profile::window_settings_path_at(root))?;
                Ok(SnippetValues::from(&current))
            })();
            match result {
                Ok(saved) => {
                    self.saved_snippets = Some(saved);
                    self.snippets = Some(saved);
                    self.snippet_update = Some(saved);
                }
                Err(error) => errors.push(format!("Snippet defaults were not saved ({error}).")),
            }
        }
        if let Some(before) = &self.history
            && self.parsed_limit() != Some(before.limit)
        {
            match self.parsed_limit().ok_or_else(|| io::Error::other("Keep between 5 and 100 prompts"))
                .and_then(|limit| history::set_limit(root, before, limit, false).map(|()| limit)) {
                Ok(limit) => {
                    let mut saved = before.clone();
                    saved.limit = limit;
                    saved.entries.truncate(limit);
                    self.history_update = Some(saved.clone());
                    self.history = Some(saved);
                }
                Err(error) => errors.push(format!("The prompt limit was not saved ({error}). Reopen AI Manager to review the current history.")),
            }
        }
        self.problem = std::iter::once(self.load_problem.as_str())
            .chain(errors.iter().map(String::as_str))
            .filter(|message| !message.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        (!self.problem.is_empty()).then(|| self.problem.clone())
    }

    pub(crate) fn take_updates(&mut self) -> (Option<SnippetValues>, Option<Snapshot>) {
        (self.snippet_update.take(), self.history_update.take())
    }
}

fn read_settings(root: &Path) -> io::Result<WindowSettings> {
    match WindowSettings::read(&crate::profile::window_settings_path_at(root)) {
        Ok(settings) => Ok(settings),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(WindowSettings::default()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests;
