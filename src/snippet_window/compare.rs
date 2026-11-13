//! Read-only comparison of the exact saved snippets selected in the library.

use std::collections::HashMap;
use std::fmt;
use std::ops::Range;
use std::sync::Arc;

use iced::widget::{self, button, column, container, row, scrollable, space, text};
use iced::{Alignment, Element, Font, Length, Task, Theme};
use serde::Serialize;

use crate::snippet_analysis::model::{ApplyRun, ChangeReason, Record};
use crate::snippets::Snippet;

pub(crate) const TITLE: &str = "Compare snippets";
const MAX_LINES: usize = 16_384;
const MAX_LINE_BYTES: usize = 4 * 1024;
const MAX_DIFF_CELLS: usize = 1_000_000;
const PAGE_ROWS: usize = 60;

#[derive(PartialEq, Eq)]
pub(crate) struct Request([Source; 2]);

#[derive(PartialEq, Eq)]
struct Source {
    label: String,
    code: Arc<str>,
}

impl Request {
    pub(super) fn new(left: &Snippet, right: &Snippet) -> Self {
        Self([left, right].map(|snippet| Source {
            label: snippet.label(),
            code: snippet.shared_content(),
        }))
    }

    pub(crate) fn texts(left: (String, String), right: (String, String)) -> Self {
        Self([left, right].map(|(label, code)| Source {
            label,
            code: code.into(),
        }))
    }

    pub(crate) fn compare(self) -> Result<Comparison, &'static str> {
        let [left, right] = self.0;
        let left = File::new(left)?;
        let right = File::new(right)?;
        let mut identifiers = HashMap::new();
        let [left_ids, right_ids] = [&left, &right].map(|file| {
            file.lines
                .iter()
                .map(|range| {
                    let next = identifiers.len();
                    *identifiers
                        .entry(&file.source.code[range.clone()])
                        .or_insert(next)
                })
                .collect::<Vec<_>>()
        });
        let rows = align(&left_ids, &right_ids)?;
        let changes = rows.iter().filter(|row| row.changed).count();
        Ok(Comparison {
            left,
            right,
            rows,
            changes,
        })
    }
}

struct File {
    source: Source,
    lines: Vec<Range<usize>>,
}

impl File {
    fn new(source: Source) -> Result<Self, &'static str> {
        let mut lines = Vec::new();
        let mut start = 0;
        if !source.code.is_empty() {
            let mut source_lines = source.code.split('\n').peekable();
            while let Some(line) = source_lines.next() {
                if lines.len() == MAX_LINES {
                    return Err("A text source exceeds the 16,384-line comparison limit.");
                }
                let shown = if source_lines.peek().is_some() {
                    line.strip_suffix('\r').unwrap_or(line)
                } else {
                    line
                };
                if shown.len() > MAX_LINE_BYTES {
                    return Err("A line exceeds the 4 KiB comparison limit.");
                }
                lines.push(start..start + shown.len());
                start += line.len() + 1;
            }
        }
        Ok(Self { source, lines })
    }

    fn line(&self, line: usize) -> &str {
        &self.source.code[self.lines[line].clone()]
    }
}

pub(crate) struct Comparison {
    left: File,
    right: File,
    rows: Vec<Aligned>,
    changes: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ChangeLocation {
    start: usize,
    end: usize,
    first_line: usize,
    last_line: usize,
    proposed: bool,
}

impl ChangeLocation {
    pub(crate) fn label(self) -> String {
        let side = if self.proposed {
            "Proposed"
        } else {
            "Original"
        };
        if self.first_line == self.last_line {
            format!("{side} line {}", self.first_line)
        } else {
            format!("{side} lines {}–{}", self.first_line, self.last_line)
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ExportDiff {
    added: usize,
    removed: usize,
    old_lines: usize,
    new_lines: usize,
    pub(crate) too_large: bool,
    pub(crate) hunks: Vec<ExportHunk>,
}

#[derive(Serialize)]
pub(crate) struct ExportHunk {
    pub(crate) header: String,
    pub(crate) findings: Vec<String>,
    pub(crate) lines: Vec<String>,
}

impl ExportHunk {
    pub(crate) fn reasons<'a>(
        &'a self,
        run: &'a ApplyRun,
    ) -> impl Iterator<Item = &'a ChangeReason> {
        run.reasons
            .iter()
            .filter(move |reason| self.findings.contains(&reason.id))
    }
}

impl ExportDiff {
    pub(crate) fn for_apply(record: &Record, run: &ApplyRun) -> Self {
        match Request::texts(
            (String::new(), run.before.clone()),
            (String::new(), run.after.clone()),
        )
        .compare()
        {
            Ok(diff) => {
                let locations: Vec<_> = run
                    .reasons
                    .iter()
                    .filter_map(|reason| {
                        diff.reason_change(record, run, reason)
                            .map(|location| (location, reason.id.as_str()))
                    })
                    .collect();
                diff.export(&locations)
            }
            Err(_) => Self {
                added: 0,
                removed: 0,
                old_lines: run.before.lines().count(),
                new_lines: run.after.lines().count(),
                too_large: true,
                hunks: Vec::new(),
            },
        }
    }

    pub(crate) fn hunks_for<'a>(&'a self, id: &'a str) -> impl Iterator<Item = usize> + 'a {
        self.hunks
            .iter()
            .enumerate()
            .filter_map(move |(index, hunk)| {
                hunk.findings
                    .iter()
                    .any(|finding| finding == id)
                    .then_some(index)
            })
    }
}

impl Comparison {
    pub(crate) fn sources(&self) -> [(&str, &str); 2] {
        [&self.left, &self.right]
            .map(|file| (file.source.label.as_str(), file.source.code.as_ref()))
    }

    pub(crate) fn export_without_findings(&self) -> ExportDiff {
        self.export(&[])
    }

    pub(crate) fn reason_change(
        &self,
        record: &Record,
        run: &ApplyRun,
        reason: &ChangeReason,
    ) -> Option<ChangeLocation> {
        if let Some(anchor) = &reason.anchor {
            return self.anchored_change(anchor);
        }
        if record.source.sha256 != run.before_sha256 {
            return None;
        }
        let line = record
            .analysis
            .improvements
            .iter()
            .find(|finding| finding.id == reason.id && finding.file.is_none())?
            .line?;
        self.original_change(line)
    }

    pub(crate) fn anchored_change(&self, anchor: &str) -> Option<ChangeLocation> {
        let anchor = anchor.trim();
        let mut matches = self.rows.iter().enumerate().filter(|(_, row)| {
            row.changed
                && row.right.is_some_and(|line| {
                    let text = self.right.line(line);
                    let text = if line == 0 {
                        text.strip_prefix('\u{feff}').unwrap_or(text)
                    } else {
                        text
                    };
                    text.trim() == anchor
                })
        });
        let location = self.change_at(matches.next()?.0)?;
        (!matches.any(|(row, _)| row >= location.end)).then_some(location)
    }

    pub(crate) fn original_change(&self, line: usize) -> Option<ChangeLocation> {
        let line = line.checked_sub(1)?;
        self.change_at(self.rows.iter().position(|row| row.left == Some(line))?)
    }

    fn change_at(&self, index: usize) -> Option<ChangeLocation> {
        if !self.rows[index].changed {
            return None;
        }
        let start = self.rows[..index]
            .iter()
            .rposition(|row| !row.changed)
            .map_or(0, |index| index + 1);
        let end = self.rows[index..]
            .iter()
            .position(|row| !row.changed)
            .map_or(self.rows.len(), |offset| index + offset);
        let span = |index: fn(&Aligned) -> Option<usize>| {
            let mut lines = self.rows[start..end].iter().filter_map(index);
            let first = lines.next()?;
            Some((first + 1, lines.next_back().unwrap_or(first) + 1))
        };
        let (first_line, last_line, proposed) = match span(|row| row.right) {
            Some((first, last)) => (first, last, true),
            None => {
                let (first, last) = span(|row| row.left)?;
                (first, last, false)
            }
        };
        Some(ChangeLocation {
            start,
            end,
            first_line,
            last_line,
            proposed,
        })
    }

    fn export(&self, locations: &[(ChangeLocation, &str)]) -> ExportDiff {
        const CONTEXT: usize = 3;
        let mut ranges: Vec<Range<usize>> = Vec::new();
        let (mut added, mut removed) = (0, 0);
        for (index, row) in self.rows.iter().enumerate().filter(|(_, row)| row.changed) {
            added += usize::from(row.right.is_some());
            removed += usize::from(row.left.is_some());
            let range = index.saturating_sub(CONTEXT)..(index + CONTEXT + 1).min(self.rows.len());
            if let Some(previous) = ranges.last_mut()
                && previous.end >= range.start
            {
                previous.end = range.end;
            } else {
                ranges.push(range);
            }
        }
        ExportDiff {
            added,
            removed,
            old_lines: self.left.lines.len(),
            new_lines: self.right.lines.len(),
            too_large: false,
            hunks: ranges
                .into_iter()
                .map(|range| {
                    let findings = locations
                        .iter()
                        .filter(|(location, _)| range.contains(&location.start))
                        .map(|(_, id)| (*id).to_owned())
                        .collect();
                    self.hunk(range, findings)
                })
                .collect(),
        }
    }

    fn hunk(&self, range: Range<usize>, findings: Vec<String>) -> ExportHunk {
        let rows = &self.rows[range.clone()];
        let span = |index: fn(&Aligned) -> Option<usize>| {
            let count = rows.iter().filter_map(index).count();
            let first = rows.iter().find_map(index).map_or_else(
                || {
                    self.rows[..range.start]
                        .iter()
                        .rev()
                        .find_map(index)
                        .map_or(0, |index| index + 1)
                },
                |index| index + 1,
            );
            format!("{first},{count}")
        };
        let mut lines = Vec::new();
        for row in rows {
            if row.changed {
                if let Some(index) = row.left {
                    lines.push(format!("-{}", self.left.line(index)));
                }
                if let Some(index) = row.right {
                    lines.push(format!("+{}", self.right.line(index)));
                }
            } else if let Some(index) = row.left {
                lines.push(format!(" {}", self.left.line(index)));
            }
        }
        ExportHunk {
            header: format!("@@ -{} +{} @@", span(|row| row.left), span(|row| row.right)),
            findings,
            lines,
        }
    }
}

impl fmt::Debug for Comparison {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnippetComparison")
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Aligned {
    left: Option<usize>,
    right: Option<usize>,
    changed: bool,
}

fn align(left: &[usize], right: &[usize]) -> Result<Vec<Aligned>, &'static str> {
    let first = left
        .iter()
        .zip(right)
        .take_while(|(left, right)| left == right)
        .count();
    let tail = left[first..]
        .iter()
        .rev()
        .zip(right[first..].iter().rev())
        .take_while(|(left, right)| left == right)
        .count();
    let mut rows = Vec::with_capacity(left.len().max(right.len()));
    rows.extend((0..first).map(|index| unchanged(index, index)));
    let left_end = left.len() - tail;
    let right_end = right.len() - tail;
    let left_middle = &left[first..left_end];
    let right_middle = &right[first..right_end];
    if left_middle.is_empty() || right_middle.is_empty() {
        changed(&mut rows, first..left_end, first..right_end);
    } else {
        let stride = right_middle.len() + 1;
        let cells = (left_middle.len() + 1) * stride;
        if cells > MAX_DIFF_CELLS {
            return Err("The differing section exceeds the line comparison work limit.");
        }
        let mut lengths = vec![0_u16; cells];
        for (left_line, left_id) in left_middle.iter().enumerate().rev() {
            for (right_line, right_id) in right_middle.iter().enumerate().rev() {
                let cell = left_line * stride + right_line;
                lengths[cell] = if left_id == right_id {
                    lengths[cell + stride + 1] + 1
                } else {
                    lengths[cell + stride].max(lengths[cell + 1])
                };
            }
        }
        let (mut l, mut r, mut from_l, mut from_r) = (0, 0, first, first);
        while l < left_middle.len() && r < right_middle.len() {
            if left_middle[l] == right_middle[r] {
                changed(&mut rows, from_l..first + l, from_r..first + r);
                rows.push(unchanged(first + l, first + r));
                l += 1;
                r += 1;
                from_l = first + l;
                from_r = first + r;
            } else {
                let cell = l * stride + r;
                if lengths[cell + stride] >= lengths[cell + 1] {
                    l += 1;
                } else {
                    r += 1;
                }
            }
        }
        changed(&mut rows, from_l..left_end, from_r..right_end);
    }
    rows.extend((0..tail).map(|index| unchanged(left_end + index, right_end + index)));
    Ok(rows)
}

fn unchanged(left: usize, right: usize) -> Aligned {
    Aligned {
        left: Some(left),
        right: Some(right),
        changed: false,
    }
}

fn changed(rows: &mut Vec<Aligned>, left: Range<usize>, right: Range<usize>) {
    for offset in 0..left.len().max(right.len()) {
        rows.push(Aligned {
            left: (offset < left.len()).then_some(left.start + offset),
            right: (offset < right.len()).then_some(right.start + offset),
            changed: true,
        });
    }
}

#[derive(Clone, Debug)]
pub enum Message {
    Loaded(Result<Arc<Comparison>, &'static str>),
    Page(usize),
    Close,
}

pub(crate) struct View {
    comparison: Option<Result<Arc<Comparison>, &'static str>>,
    page: usize,
    scroll: widget::Id,
    focus_anchor: widget::Id,
    focused: Option<ChangeLocation>,
}

impl Default for View {
    fn default() -> Self {
        Self {
            comparison: None,
            page: 0,
            scroll: widget::Id::unique(),
            focus_anchor: widget::Id::unique(),
            focused: None,
        }
    }
}

impl View {
    pub(crate) fn reveal_change(&mut self, location: ChangeLocation) -> Task<Message> {
        self.focused = Some(location);
        self.page = location.start / PAGE_ROWS;
        crate::settings_search::scroll_to_row(self.scroll.clone(), self.focus_anchor.clone())
    }

    pub(crate) fn clear_change_focus(&mut self) {
        self.focused = None;
    }

    pub(crate) fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Loaded(result) if self.comparison.is_none() => self.comparison = Some(result),
            Message::Page(page) if page < self.pages() && page != self.page => {
                self.page = page;
                return widget::operation::scroll_to(
                    self.scroll.clone(),
                    widget::operation::AbsoluteOffset { x: 0.0, y: 0.0 },
                );
            }
            _ => {}
        }
        Task::none()
    }

    fn pages(&self) -> usize {
        match &self.comparison {
            Some(Ok(comparison)) => comparison.rows.len().div_ceil(PAGE_ROWS).max(1),
            _ => 1,
        }
    }

    pub(crate) fn view(&self) -> Element<'_, Message> {
        column![
            self.review_body(),
            row![
                space::horizontal(),
                button("Close").on_press(Message::Close)
            ]
            .spacing(8),
        ]
        .padding(12)
        .spacing(8)
        .height(Length::Fill)
        .into()
    }

    pub(crate) fn review_body(&self) -> Element<'_, Message> {
        match &self.comparison {
            Some(Ok(diff)) => self.diff(diff),
            Some(Err(error)) => container(text(*error)).center(Length::Fill).into(),
            None => container(text("Comparing saved snippets…"))
                .center(Length::Fill)
                .into(),
        }
    }

    fn diff<'a>(&'a self, diff: &'a Comparison) -> Element<'a, Message> {
        let pages = self.pages();
        let rows = diff
            .rows
            .iter()
            .enumerate()
            .skip(self.page * PAGE_ROWS)
            .take(PAGE_ROWS)
            .fold(column![].spacing(1), |rows, (index, line)| {
                let focus = self
                    .focused
                    .map(|change| (change.start..change.end).contains(&index));
                let cells = row![
                    cell(&diff.left, line.left, line.changed, true, focus),
                    cell(&diff.right, line.right, line.changed, false, focus),
                ]
                .spacing(6);
                if self.focused.is_some_and(|change| change.start == index) {
                    rows.push(
                        container(cells)
                            .id(self.focus_anchor.clone())
                            .width(Length::Fill),
                    )
                } else {
                    rows.push(cells)
                }
            });
        column![
            row![
                text(&diff.left.source.label).width(Length::FillPortion(1)),
                text(&diff.right.source.label).width(Length::FillPortion(1)),
            ]
            .spacing(6),
            text(if diff.changes == 0 {
                "No line differences".into()
            } else {
                format!("{} differing rows", diff.changes)
            }),
            scrollable(rows)
                .id(self.scroll.clone())
                .height(Length::Fill),
            row![
                button("Previous").on_press_maybe(self.page.checked_sub(1).map(Message::Page)),
                text(format!("{} / {pages}", self.page + 1)),
                button("Next").on_press_maybe(
                    (self.page + 1 < pages).then_some(Message::Page(self.page + 1))
                ),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        ]
        .spacing(8)
        .height(Length::Fill)
        .into()
    }
}

fn cell(
    file: &File,
    index: Option<usize>,
    changed: bool,
    left: bool,
    focus: Option<bool>,
) -> Element<'_, Message> {
    let label = index.map_or_else(String::new, |index| format!("{:>5}", index + 1));
    let content = index.map_or("", |index| file.line(index));
    let marker = if changed && index.is_some() {
        if left { "−" } else { "+" }
    } else {
        " "
    };
    container(
        row![
            text(label).font(Font::MONOSPACE).size(12),
            text(marker).font(Font::MONOSPACE).size(12),
            text(content)
                .font(Font::MONOSPACE)
                .size(13)
                .width(Length::Fill),
        ]
        .spacing(6),
    )
    .width(Length::FillPortion(1))
    .padding([1, 4])
    .style(move |theme: &Theme| {
        let palette = theme.extended_palette();
        let background = if changed {
            Some(if focus == Some(true) {
                palette.primary.weak.color
            } else if index.is_none() || focus == Some(false) {
                palette.background.weak.color
            } else if left {
                palette.danger.weak.color
            } else {
                palette.success.weak.color
            })
        } else {
            None
        };
        container::Style {
            background: background.map(Into::into),
            ..container::Style::default()
        }
    })
    .into()
}

#[cfg(test)]
mod tests;
