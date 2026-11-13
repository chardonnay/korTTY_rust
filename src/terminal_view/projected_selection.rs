//! Clipboard selections of rendered rows, independent of raw terminal ranges.

use std::sync::{Arc, Weak};

use crate::session::Frame;
use crate::terminal_agent::design::inline::Line;

use super::Selection;

pub(crate) struct View<'a> {
    pub source: &'a Arc<Frame>,
    pub stream: u64,
    pub range: Option<Selection>,
}

#[derive(Clone, Debug)]
pub struct ProjectedSelection {
    source: Weak<Frame>,
    stream: u64,
    range: Selection,
}

impl ProjectedSelection {
    pub(crate) fn new(source: &Arc<Frame>, stream: u64, range: Selection) -> Self {
        Self {
            source: Arc::downgrade(source),
            stream,
            range,
        }
    }

    pub(crate) fn range(&self, frame: &Arc<Frame>, stream: u64) -> Option<Selection> {
        (self.stream == stream && self.source.ptr_eq(&Arc::downgrade(frame))).then_some(self.range)
    }

    pub(crate) fn retain(&mut self, previous: &Arc<Frame>, next: &Arc<Frame>, stream: u64) -> bool {
        if self.range(previous, stream).is_none() || !same_rows(previous, next, self.range) {
            return false;
        }
        self.source = Arc::downgrade(next);
        true
    }

    pub(crate) fn text(&self, frame: &Arc<Frame>, stream: u64) -> Option<String> {
        let range = self.range(frame, stream)?;
        let projection = frame.inline.as_deref()?;
        let (first, last) = range.bounds();
        let mut result = String::new();
        for display in first.row..=last.row.min(frame.screen.rows.saturating_sub(1)) {
            if display != first.row {
                result.push('\n');
            }
            let (start, end) = columns(range, display, frame.screen.columns);
            let text = if let Some(raw) = frame.display_raw_row(display) {
                let Some(cells) = frame.screen.row(raw) else {
                    continue;
                };
                // A click on the continuation still belongs to its wide glyph.
                let start = if cells
                    .get(usize::from(start))
                    .is_some_and(|cell| cell.width == 0)
                {
                    start.saturating_sub(1)
                } else {
                    start
                };
                Selection::from_cells((raw, start), (raw, end)).visible_text(&frame.screen)
            } else if let Some(line) = projection.line(display) {
                annotation_text(line, start, end)
            } else {
                continue;
            };
            result.extend(text.chars().filter(|character| !character.is_control()));
        }
        Some(result)
    }
}

pub(super) fn same_rows(previous: &Arc<Frame>, next: &Arc<Frame>, selection: Selection) -> bool {
    if Arc::ptr_eq(previous, next) {
        return true;
    }
    let (Some(old), Some(new)) = (previous.inline.as_deref(), next.inline.as_deref()) else {
        return false;
    };
    if previous.screen.columns != next.screen.columns || previous.screen.rows != next.screen.rows {
        return false;
    }
    let old_start = previous
        .screen
        .scrollback_lines
        .saturating_sub(previous.scroll_offset);
    let new_start = next
        .screen
        .scrollback_lines
        .saturating_sub(next.scroll_offset);
    let (first, last) = selection.bounds();
    (first.row..=last.row).all(|row| {
        match (previous.display_raw_row(row), next.display_raw_row(row)) {
            (Some(left), Some(right)) => {
                old_start + usize::from(left) == new_start + usize::from(right)
                    && previous.screen.row(left) == next.screen.row(right)
            }
            (None, None) => {
                old.line_identity(row) == new.line_identity(row)
                    && old
                        .line(row)
                        .is_some_and(|line| new.line(row) == Some(line))
            }
            _ => false,
        }
    })
}

fn columns(selection: Selection, row: u16, width: u16) -> (u16, u16) {
    let (first, last) = selection.bounds();
    let start = if first.row == row { first.column } else { 0 };
    let end = if last.row == row {
        last.column
    } else {
        width.saturating_sub(1)
    };
    (
        start.min(width.saturating_sub(1)),
        end.min(width.saturating_sub(1)),
    )
}

fn annotation_text(line: &Line, start: u16, end: u16) -> String {
    let mut result = String::new();
    let mut column = 0;
    let mut selected = false;
    for character in line
        .segments
        .iter()
        .flat_map(|segment| segment.text.chars())
    {
        let Some(width) = kortty_terminal::character_cell_width(character) else {
            continue;
        };
        if width != 0 {
            let next = column + width;
            selected = column <= usize::from(end) && next > usize::from(start);
            column = next;
        }
        if selected {
            result.push(character);
        }
    }
    result.truncate(result.trim_end_matches(' ').len());
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::inline_history;
    use crate::terminal_agent::design::inline::{Role, Segment};
    use crate::terminal_view::selection_test_frame;

    fn frame() -> Arc<Frame> {
        let mut terminal = kortty_terminal::Terminal::new(24, 6);
        terminal.advance(b"before\x1b[2;1H\x1b[8msecret\x1b[28m visible");
        let mut frame = selection_test_frame(terminal.snapshot());
        inline_history::fixture(
            Arc::get_mut(&mut frame).unwrap(),
            1,
            vec![Line {
                segments: vec![Segment {
                    text: "A界\u{301}B".into(),
                    role: Role::Text,
                }],
            }],
            false,
        );
        frame
    }

    #[test]
    fn projected_copy_keeps_wide_and_combining_text_and_excludes_concealed_cells() {
        let frame = frame();
        let selection = ProjectedSelection::new(&frame, 4, Selection::from_cells((1, 2), (2, 23)));
        assert_eq!(
            selection.text(&frame, 4).as_deref(),
            Some("界\u{301}B\n       visible")
        );
        assert!(selection.text(&frame, 5).is_none());
        let other = Arc::new(frame.as_ref().clone());
        assert!(selection.text(&other, 4).is_none());
    }

    #[test]
    fn projected_selection_can_rebind_only_when_its_rendered_rows_match() {
        let frame = frame();
        let range = Selection::from_cells((1, 0), (2, 23));
        let mut selection = ProjectedSelection::new(&frame, 4, range);
        let mut next = frame.as_ref().clone();
        next.screen.cells[0].text.clear();
        let next = Arc::new(next);
        assert!(selection.retain(&frame, &next, 4));
        assert_eq!(selection.range(&next, 4), Some(range));

        let mut changed = next.as_ref().clone();
        changed.screen.cells[31].text.clear();
        let changed = Arc::new(changed);
        assert!(!selection.retain(&next, &changed, 4));
        assert!(selection.text(&changed, 4).is_none());
    }
}
