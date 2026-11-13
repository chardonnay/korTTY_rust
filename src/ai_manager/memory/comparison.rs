use iced::{
    Element, Font, Length,
    widget::{column, row, scrollable, text},
};

// Keep the comparison usable even for files with thousands of short lines.
const MAX_LCS_CELLS: usize = 256 * 1024;

pub(super) struct Comparison {
    current: String,
    proposed: String,
    grouped: bool,
}

impl Comparison {
    pub(super) fn new(current: &str, proposed: &str) -> Self {
        let before: Vec<_> = current.split_inclusive('\n').collect();
        let after: Vec<_> = proposed.split_inclusive('\n').collect();
        let prefix = before
            .iter()
            .zip(&after)
            .take_while(|(a, b)| a == b)
            .count();
        let suffix = before[prefix..]
            .iter()
            .rev()
            .zip(after[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        let before_end = before.len() - suffix;
        let after_end = after.len() - suffix;
        let mut result = Self {
            current: String::new(),
            proposed: String::new(),
            grouped: false,
        };
        for line in &before[..prefix] {
            result.unchanged(line);
        }
        result.changed(&before[prefix..before_end], &after[prefix..after_end]);
        for line in &before[before_end..] {
            result.unchanged(line);
        }
        result
    }

    fn unchanged(&mut self, line: &str) {
        append(&mut self.current, "  ", line);
        append(&mut self.proposed, "  ", line);
    }

    fn changed(&mut self, before: &[&str], after: &[&str]) {
        let width = after.len() + 1;
        let cells = (before.len() + 1).saturating_mul(width);
        if before.is_empty() || after.is_empty() {
            self.block(before, after);
            return;
        }
        if cells > MAX_LCS_CELLS {
            // Batch archival removes complete lines. Preserve the unchanged
            // regions even in a file with many short lines.
            if !self.removals(before, after) {
                self.grouped = true;
                self.block(before, after);
            }
            return;
        }

        let mut lengths = vec![0u16; cells];
        for (i, old) in before.iter().enumerate().rev() {
            for (j, new) in after.iter().enumerate().rev() {
                let here = i * width + j;
                lengths[here] = if old == new {
                    lengths[here + width + 1] + 1
                } else {
                    lengths[here + 1].max(lengths[here + width])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        let (mut start_i, mut start_j) = (0, 0);
        while i < before.len() && j < after.len() {
            if before[i] == after[j] {
                self.block(&before[start_i..i], &after[start_j..j]);
                self.unchanged(before[i]);
                i += 1;
                j += 1;
                (start_i, start_j) = (i, j);
            } else if lengths[(i + 1) * width + j] >= lengths[i * width + j + 1] {
                i += 1;
            } else {
                j += 1;
            }
        }
        self.block(&before[start_i..], &after[start_j..]);
    }

    fn removals(&mut self, before: &[&str], after: &[&str]) -> bool {
        let mut remaining = before.iter();
        if !after
            .iter()
            .all(|line| remaining.any(|previous| previous == line))
        {
            return false;
        }
        let mut start = 0;
        for line in after {
            let count = before[start..]
                .iter()
                .take_while(|previous| *previous != line)
                .count();
            self.block(&before[start..start + count], &[]);
            self.unchanged(line);
            start += count + 1;
        }
        self.block(&before[start..], &[]);
        true
    }

    fn block(&mut self, before: &[&str], after: &[&str]) {
        for index in 0..before.len().max(after.len()) {
            if let Some(line) = before.get(index) {
                append(&mut self.current, "− ", line);
            } else {
                self.current.push('\n');
            }
            if let Some(line) = after.get(index) {
                append(&mut self.proposed, "+ ", line);
            } else {
                self.proposed.push('\n');
            }
        }
    }

    pub(super) fn view<'a, Message: 'a>(
        &'a self,
        label: &'static str,
        height: Length,
    ) -> Element<'a, Message> {
        column![
            self.grouped.then(|| text("Large change regions are grouped. Matching lines within them may also have change markers.").size(13)),
            row![panel("Current file", &self.current, height), panel(label, &self.proposed, height)].spacing(12),
        ].spacing(6).into()
    }
}

fn append(output: &mut String, marker: &str, line: &str) {
    output.push_str(marker);
    if let Some(text) = line.strip_suffix("\r\n") {
        output.push_str(text);
        if marker != "  " {
            output.push_str("  [CRLF]");
        }
    } else if let Some(text) = line.strip_suffix('\n') {
        output.push_str(text);
    } else {
        output.push_str(line);
        output.push_str("  [no final newline]");
    }
    output.push('\n');
}

fn panel<'a, Message: 'a>(
    title: &'static str,
    content: &'a str,
    height: Length,
) -> Element<'a, Message> {
    column![
        text(title).size(14),
        scrollable(
            text(content)
                .font(Font::MONOSPACE)
                .size(13)
                .wrapping(text::Wrapping::None)
        )
        .direction(scrollable::Direction::Both {
            vertical: scrollable::Scrollbar::new(),
            horizontal: scrollable::Scrollbar::new(),
        })
        .width(Length::Fill)
        .height(height),
    ]
    .width(Length::Fill)
    .spacing(6)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_comparison_aligns_unchanged_lines_and_shows_line_ending_edits() {
        let comparison = Comparison::new("α\nold\nkeep\nend\r\n", "α\nnew\nextra\nkeep\nend");
        assert_eq!(comparison.current, "  α\n− old\n\n  keep\n− end  [CRLF]\n");
        assert_eq!(
            comparison.proposed,
            "  α\n+ new\n+ extra\n  keep\n+ end  [no final newline]\n"
        );

        let large = Comparison::new(&"a\n".repeat(2_000), &"b\n".repeat(2_000));
        assert_eq!(large.current.lines().count(), 2_000);
        assert_eq!(large.proposed.lines().last(), Some("+ b"));
        assert!(large.grouped);

        let keep = "user note\n".repeat(1_000);
        let archive = Comparison::new(&format!("one\n{keep}two\n"), &keep);
        assert!(!archive.grouped);
        assert_eq!(
            archive
                .current
                .lines()
                .filter(|line| line.starts_with('−'))
                .count(),
            2
        );
    }
}
