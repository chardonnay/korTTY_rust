use super::*;

pub(in crate::snippet_window) struct Inline {
    context: Local,
    ticket: u64,
    suggestions: Vec<(usize, String)>,
    selected: usize,
}

impl Inline {
    pub(in crate::snippet_window) fn open(
        content: &text_editor::Content,
        language: &str,
    ) -> Option<Self> {
        if !at_line_end(content) {
            return None;
        }
        Some(Self {
            context: Local::open(content, language)?,
            ticket: 0,
            suggestions: Vec::new(),
            selected: 0,
        })
    }

    pub(in crate::snippet_window) fn target(
        &self,
        source: &ai_assist::Snapshot,
    ) -> Option<ai_assist::CompletionTarget> {
        let mut target = self.context.target(source)?;
        target.inline = true;
        Some(target)
    }

    pub(in crate::snippet_window) fn current(&self, target: &ai_assist::CompletionTarget) -> bool {
        self.context.current(target)
    }

    pub(in crate::snippet_window) fn preview(&self) -> Option<&str> {
        self.suggestions
            .get(self.selected)
            .map(|(_, preview)| preview.as_str())
    }

    pub(in crate::snippet_window) fn matches(&self, ticket: u64, index: usize) -> bool {
        self.ticket == ticket
            && self
                .suggestions
                .get(self.selected)
                .is_some_and(|(selected, _)| *selected == index)
    }

    pub(in crate::snippet_window) fn publish(
        &mut self,
        ticket: u64,
        candidates: &[ai_assist::Alternative],
    ) -> usize {
        let prefix = &self.context.line[self.context.range.clone()];
        self.ticket = ticket;
        self.selected = 0;
        self.suggestions = candidates
            .iter()
            .enumerate()
            .filter_map(|(index, candidate)| {
                let suffix = candidate.outcome.proposed_text().strip_prefix(prefix)?;
                if suffix.is_empty() {
                    return None;
                }
                Some((index, suffix.replace("\r\n", "\n").replace('\r', "\n")))
            })
            .collect();
        self.suggestions.len()
    }

    pub(in crate::snippet_window) fn step(&mut self, forward: bool) {
        let count = self.suggestions.len();
        if count > 1 {
            self.selected = (self.selected + if forward { 1 } else { count - 1 }) % count;
        }
    }

    pub(in crate::snippet_window) fn clear(&mut self) {
        self.suggestions.clear();
        self.selected = 0;
    }

    pub(in crate::snippet_window) fn key(&self, event: &text_editor::KeyPress) -> Option<Message> {
        use iced::keyboard::{
            Key, Modifiers,
            key::{Code, Named, Physical},
        };
        if !matches!(event.status, text_editor::Status::Focused { .. }) {
            return None;
        }
        if event.modifiers.is_empty() && event.key == Key::Named(Named::Escape) {
            return Some(Message::Assist(ai_assist::Message::StopInlineCompletion));
        }
        let (index, _) = self.suggestions.get(self.selected)?;
        match (event.physical_key, event.modifiers) {
            (_, modifiers) if modifiers.is_empty() && event.key == Key::Named(Named::Tab) => Some(
                Message::Assist(ai_assist::Message::ApplyCompletion(self.ticket, *index)),
            ),
            (Physical::Code(Code::BracketRight), Modifiers::ALT) => {
                Some(Message::StepInlineCompletion(true))
            }
            (Physical::Code(Code::BracketLeft), Modifiers::ALT) => {
                Some(Message::StepInlineCompletion(false))
            }
            _ => None,
        }
    }
}
