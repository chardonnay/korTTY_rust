/// Direction of a jump among shell integration prompt marks.
#[derive(Clone, Copy, Debug)]
pub enum PromptDirection {
    Previous,
    Next,
}

#[derive(Clone, Copy)]
pub(crate) struct LineMark {
    pub(crate) id: u64,
    pub(crate) column: usize,
    pub(crate) kind: LineMarkKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum LineMarkKind {
    Prompt,
    OutputStart,
    OutputEnd,
    Timestamp(SystemTime),
}

impl LineMarkKind {
    pub(crate) fn is_timestamp(self) -> bool {
        matches!(self, Self::Timestamp(_))
    }
}

/// Positions of the prompts in one terminal frame, independent of viewport scroll.
#[derive(Debug)]
pub struct PromptLayout {
    pub(crate) history: usize,
    pub(crate) cursor_line: usize,
    pub(crate) epoch: u64,
    pub(crate) current: Option<usize>,
    pub(crate) rows: Vec<usize>,
}

/// Tracks successive prompt jumps while the cursor and viewport stay put.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PromptNavigator {
    last: Option<Jump>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Jump {
    target_row: usize,
    scroll_offset: usize,
    cursor_line: usize,
    epoch: u64,
}

impl PromptNavigator {
    /// Returns the new viewport offset. The layout must come from the same terminal.
    pub fn navigate(
        &mut self,
        layout: &PromptLayout,
        direction: PromptDirection,
        scroll_offset: usize,
    ) -> usize {
        let last_row = self.last.filter(|last| {
            last.scroll_offset == scroll_offset
                && last.cursor_line == layout.cursor_line
                && last.epoch == layout.epoch
                && layout.rows.binary_search(&last.target_row).is_ok()
        });
        let reference = last_row.map_or_else(
            || {
                if scroll_offset == 0 {
                    layout
                        .current
                        .map_or(layout.cursor_line, |row| row.min(layout.cursor_line))
                } else {
                    layout.history.saturating_sub(scroll_offset)
                }
            },
            |last| last.target_row,
        );
        let target = match direction {
            PromptDirection::Previous => {
                layout.rows[..layout.rows.partition_point(|&row| row < reference)].last()
            }
            PromptDirection::Next => layout
                .rows
                .get(layout.rows.partition_point(|&row| row <= reference)),
        };
        if let Some(&row) = target {
            let scroll_offset = layout.history.saturating_sub(row);
            self.last = Some(Jump {
                target_row: row,
                scroll_offset,
                cursor_line: layout.cursor_line,
                epoch: layout.epoch,
            });
            scroll_offset
        } else {
            match direction {
                PromptDirection::Previous => scroll_offset,
                PromptDirection::Next => {
                    self.last = None;
                    0
                }
            }
        }
    }
}
use std::time::SystemTime;
