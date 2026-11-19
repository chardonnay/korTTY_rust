//! Editable fields in one inserted completion on a single line.

use std::ops::Range;

use iced::widget::text_editor::{Content, Cursor, Position};

pub(in crate::snippet_window) struct Fields {
    line_index: usize,
    line_count: usize,
    line: String,
    fields: Vec<Range<usize>>,
    active: usize,
    end: usize,
}

impl Fields {
    pub(super) fn new(
        line_index: usize,
        line_count: usize,
        line: String,
        fields: Vec<Range<usize>>,
        end: usize,
    ) -> Self {
        Self {
            line_index,
            line_count,
            line,
            fields,
            active: 0,
            end,
        }
    }

    pub(in crate::snippet_window) fn activate(&self, content: &mut Content) -> bool {
        let Some(line) = content.line(self.line_index) else {
            return false;
        };
        if content.line_count() != self.line_count || line.text != self.line {
            return false;
        }
        content.move_to(self.cursor(self.fields[self.active].clone()));
        true
    }

    /// Preserve later fields when editing within the active field.
    pub(in crate::snippet_window) fn refresh(&mut self, content: &Content) -> bool {
        let Some(line) = content.line(self.line_index) else {
            return false;
        };
        if content.line_count() != self.line_count {
            return false;
        }
        let range = self.fields[self.active].clone();
        let before = &self.line[..range.start];
        let after = &self.line[range.end..];
        if line.text.len() < before.len() + after.len()
            || !line.text.starts_with(before)
            || !line.text.ends_with(after)
        {
            return false;
        }
        let new_end = line.text.len() - after.len();
        let within = |position: Position| {
            position.line == self.line_index && (range.start..=new_end).contains(&position.column)
        };
        let cursor = content.cursor();
        if !within(cursor.position) || cursor.selection.is_some_and(|selection| !within(selection))
        {
            return false;
        }
        if line.text != self.line {
            self.fields[self.active].end = new_end;
            for field in &mut self.fields[self.active + 1..] {
                *field = new_end + (field.start - range.end)..new_end + (field.end - range.end);
            }
            self.end = new_end + (self.end - range.end);
            self.line = line.text.to_string();
        }
        true
    }

    pub(in crate::snippet_window) fn step(&mut self, content: &mut Content, forward: bool) -> bool {
        if !self.refresh(content) {
            return false;
        }
        if forward && self.active + 1 == self.fields.len() {
            content.move_to(self.cursor(self.end..self.end));
            return false;
        }
        self.active = if forward {
            self.active + 1
        } else {
            self.active.saturating_sub(1)
        };
        content.move_to(self.cursor(self.fields[self.active].clone()));
        true
    }

    fn cursor(&self, range: Range<usize>) -> Cursor {
        let position = |column| Position {
            line: self.line_index,
            column,
        };
        Cursor {
            position: position(range.end),
            selection: (!range.is_empty()).then(|| position(range.start)),
        }
    }
}
