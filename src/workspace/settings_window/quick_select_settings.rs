use iced::widget::{self, column, text, text_editor, text_input};
use iced::{Element, Font, Task, window};

use crate::terminal_view::quick_select::{
    DEFAULT_ALPHABET, MAX_PATTERN_BYTES, MAX_PATTERNS, Preferences,
};
use crate::workspace::journal_access::entry_editor::edit_text_limited;

use super::{Desktop, Dialog, Message, Setting, highlighted};

const MAX_ALPHABET_DRAFT_BYTES: usize = 128;
const MAX_PATTERN_DRAFT_BYTES: usize = (MAX_PATTERNS + 1) * (MAX_PATTERN_BYTES + 1);

#[derive(Clone, Debug)]
pub enum Edit {
    Alphabet(String),
    Patterns(text_editor::Action),
}

pub(super) struct Editor {
    alphabet: String,
    alphabet_id: widget::Id,
    alphabet_problem: Option<String>,
    patterns: text_editor::Content,
    patterns_id: widget::Id,
    patterns_text: String,
    lines: Vec<Pattern>,
    too_many: bool,
}

struct Pattern {
    line: usize,
    source: String,
    problem: Option<String>,
}

impl Editor {
    pub(super) fn new(preferences: &Preferences) -> Self {
        let alphabet = if preferences.alphabet == DEFAULT_ALPHABET {
            String::new()
        } else {
            preferences.alphabet.clone()
        };
        let patterns_text = preferences.patterns.join("\n");
        let mut editor = Self {
            alphabet_problem: Preferences::validate_alphabet(effective_alphabet(&alphabet))
                .err()
                .map(|error| error.to_string()),
            alphabet,
            alphabet_id: widget::Id::unique(),
            patterns: text_editor::Content::with_text(&patterns_text),
            patterns_id: widget::Id::unique(),
            patterns_text,
            lines: Vec::new(),
            too_many: false,
        };
        editor.validate_patterns();
        editor
    }

    pub(super) fn update(&mut self, edit: Edit) {
        match edit {
            Edit::Alphabet(mut alphabet) => {
                alphabet.truncate(alphabet.floor_char_boundary(MAX_ALPHABET_DRAFT_BYTES));
                self.alphabet_problem =
                    Preferences::validate_alphabet(effective_alphabet(&alphabet))
                        .err()
                        .map(|error| error.to_string());
                self.alphabet = alphabet;
            }
            Edit::Patterns(action) => {
                if edit_text_limited(
                    &mut self.patterns,
                    &mut self.patterns_text,
                    action,
                    MAX_PATTERN_DRAFT_BYTES,
                ) {
                    self.validate_patterns();
                }
            }
        }
    }

    fn validate_patterns(&mut self) {
        let previous = std::mem::take(&mut self.lines);
        let mut lines = physical_lines(&self.patterns_text)
            .enumerate()
            .filter(|(_, source)| !source.chars().all(java_whitespace));
        self.lines = lines
            .by_ref()
            .take(MAX_PATTERNS)
            .map(|(line, source)| {
                let problem = previous
                    .iter()
                    .find(|old| old.source == source)
                    .map_or_else(
                        || {
                            Preferences::validate_pattern(source)
                                .err()
                                .map(|error| error.to_string())
                        },
                        |old| old.problem.clone(),
                    );
                Pattern {
                    line: line + 1,
                    source: source.to_owned(),
                    problem,
                }
            })
            .collect();
        self.too_many = lines.next().is_some();
    }

    pub(super) fn first_problem(&self) -> Option<Setting> {
        if self.alphabet_problem.is_some() {
            Some(Setting::QuickSelectAlphabet)
        } else if self.too_many || self.lines.iter().any(|line| line.problem.is_some()) {
            Some(Setting::QuickSelectPatterns)
        } else {
            None
        }
    }

    pub(super) fn focus(&self, setting: Setting) -> Task<Desktop> {
        let id = match setting {
            Setting::QuickSelectAlphabet => &self.alphabet_id,
            Setting::QuickSelectPatterns => &self.patterns_id,
            _ => return Task::none(),
        };
        widget::operation::focus(id.clone())
    }

    pub(super) fn merge(&self, initial: &Preferences, next: &mut Preferences) {
        let alphabet = effective_alphabet(&self.alphabet);
        if alphabet != effective_alphabet(&initial.alphabet) {
            next.alphabet = alphabet.to_owned();
        }
        if self
            .lines
            .iter()
            .map(|pattern| &pattern.source)
            .ne(initial.patterns.iter())
        {
            next.patterns = self
                .lines
                .iter()
                .map(|pattern| pattern.source.clone())
                .collect();
        }
    }

    pub(super) fn view(&self, dialog: &Dialog) -> Element<'_, Desktop> {
        let id = dialog.id;
        let label = |setting: Setting| text(setting.label()).size(13);
        let mut errors = column![].spacing(3);
        if self.too_many {
            errors = errors.push(problem(format!("At most {MAX_PATTERNS} patterns.")));
        }
        for line in &self.lines {
            if let Some(error) = &line.problem {
                errors = errors.push(problem(format!("Line {}: {error}", line.line)));
            }
        }
        column![
            highlighted(dialog, Setting::QuickSelectAlphabet, column![
                label(Setting::QuickSelectAlphabet),
                text_input(DEFAULT_ALPHABET, &self.alphabet)
                    .id(self.alphabet_id.clone())
                    .width(310)
                    .on_input(move |value| wrap(id, Edit::Alphabet(value))),
                self.alphabet_problem.as_deref().map(problem),
            ].spacing(4)),
            highlighted(dialog, Setting::QuickSelectPatterns, column![
                label(Setting::QuickSelectPatterns),
                text_editor(&self.patterns)
                    .id(self.patterns_id.clone())
                    .placeholder("One regular expression per line, for example [A-Z]+-\\d+")
                    .font(Font::MONOSPACE)
                    .height(104)
                    .wrapping(text::Wrapping::None)
                    .on_action(move |action| wrap(id, Edit::Patterns(action))),
                errors,
            ].spacing(4).max_width(560)),
            text("Edit → Quick Select labels addresses, paths and other built-in matches, plus matches of your own patterns. Use Rust-compatible regular expressions. Changes apply the next time Quick Select starts.").size(13),
        ].spacing(5).into()
    }
}

fn wrap(id: window::Id, edit: Edit) -> Desktop {
    Desktop::Settings(Message::QuickSelect(id, edit))
}

fn problem<'a>(message: impl text::IntoFragment<'a>) -> text::Text<'a> {
    text(message).style(text::danger).size(13)
}

fn effective_alphabet(text: &str) -> &str {
    let alphabet = text.trim_matches(java_whitespace);
    if alphabet.is_empty() {
        DEFAULT_ALPHABET
    } else {
        alphabet
    }
}

fn java_whitespace(character: char) -> bool {
    matches!(character, '\u{1c}'..='\u{1f}')
        || (character.is_whitespace()
            && !matches!(character, '\u{85}' | '\u{a0}' | '\u{2007}' | '\u{202f}'))
}

fn physical_lines(text: &str) -> impl Iterator<Item = &str> {
    let mut remaining = Some(text);
    std::iter::from_fn(move || {
        let input = remaining.take()?;
        let Some(index) = input.find(|ch| {
            matches!(
                ch,
                '\n' | '\r' | '\u{b}' | '\u{c}' | '\u{85}' | '\u{2028}' | '\u{2029}'
            )
        }) else {
            return Some(input);
        };
        let (line, suffix) = input.split_at(index);
        remaining = suffix
            .strip_prefix("\r\n")
            .or_else(|| suffix.chars().next().map(|ch| &suffix[ch.len_utf8()..]));
        Some(line)
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;

    use super::super::{Section, WindowSettings, Workspace, search_matches};
    use super::*;

    fn edit(workspace: &mut Workspace, id: window::Id, action: Edit) {
        let _ = workspace.update_settings(Message::QuickSelect(id, action));
    }

    fn patterns(workspace: &mut Workspace, id: window::Id, value: &str) {
        edit(
            workspace,
            id,
            Edit::Patterns(text_editor::Action::SelectAll),
        );
        edit(
            workspace,
            id,
            Edit::Patterns(text_editor::Action::Edit(text_editor::Edit::Paste(
                Arc::new(value.into()),
            ))),
        );
    }

    #[test]
    fn quick_select_physical_lines_keep_literal_spaces_and_skip_java_blank_lines() {
        let input = "  Ticket-[0-9]+  \r\n \t\r\n\u{85} IP-[0-9]+\u{2028}\u{a0}";
        let lines = physical_lines(input)
            .enumerate()
            .filter(|(_, line)| !line.chars().all(java_whitespace))
            .collect::<Vec<_>>();
        assert_eq!(
            lines,
            [(0, "  Ticket-[0-9]+  "), (3, " IP-[0-9]+"), (4, "\u{a0}")]
        );
    }

    #[test]
    fn quick_select_settings_validate_and_only_save_a_complete_draft() -> std::io::Result<()> {
        let profile = crate::snippets::tests::Profile::new();
        let path = profile.0.join("window-settings.json");
        let mut workspace = Workspace::default();
        let legacy: WindowSettings = serde_json::from_str("{}")?;
        assert!(legacy.quick_select.is_default());
        workspace.window_settings.save(&path)?;
        let original = fs::read(&path)?;
        let (owner, _) = workspace.open_window(false);
        let open = |workspace: &mut Workspace| {
            let _ = workspace.update_settings(Message::OpenAt(owner, Section::Terminal));
            workspace.settings.0.as_ref().unwrap().id
        };
        assert_eq!(
            search_matches("quick select").collect::<Vec<_>>(),
            [Setting::QuickSelectAlphabet, Setting::QuickSelectPatterns]
        );
        let id = open(&mut workspace);
        edit(&mut workspace, id, Edit::Alphabet("AA".into()));
        patterns(&mut workspace, id, "INC-[0-9]+\n\n[unclosed");
        let _ = workspace.update_settings(Message::Section(id, Section::Font));
        let _ =
            workspace.save_settings_to(id, |_| panic!("invalid quick select reached persistence"));
        let dialog = workspace.settings.0.as_ref().unwrap();
        assert_eq!(dialog.section, Section::Terminal);
        assert_eq!(dialog.revealed, Some(Setting::QuickSelectAlphabet));
        edit(&mut workspace, id, Edit::Alphabet("  zx  ".into()));
        let _ = workspace.save_settings_to(id, |_| panic!("invalid pattern reached persistence"));
        let dialog = workspace.settings.0.as_ref().unwrap();
        assert_eq!(dialog.revealed, Some(Setting::QuickSelectPatterns));
        assert_eq!(dialog.quick_select.lines[1].line, 3);
        assert!(dialog.quick_select.lines[1].problem.is_some());
        patterns(&mut workspace, id, " INC-[0-9]+ \n  \n[0-9]+-DEV");
        let _ = workspace.update_settings(Message::Cancel(id));
        assert_eq!(fs::read(&path)?, original);
        assert!(workspace.window_settings.quick_select.is_default());

        let id = open(&mut workspace);
        edit(&mut workspace, id, Edit::Alphabet("  zx  ".into()));
        patterns(&mut workspace, id, " INC-[0-9]+ \n  \n[0-9]+-DEV");
        let _ = workspace.save_settings_to(id, |settings| settings.save(&path));
        assert!(workspace.settings.title(id).is_none());
        let bytes = fs::read(path)?;
        WindowSettings::validate_backup(&bytes)?;
        let saved: WindowSettings = serde_json::from_slice(&bytes)?;
        assert_eq!(saved.quick_select, workspace.window_settings.quick_select);
        assert_eq!(saved.quick_select.alphabet, "zx");
        assert_eq!(saved.quick_select.patterns, [" INC-[0-9]+ ", "[0-9]+-DEV"]);
        Ok(())
    }
}
