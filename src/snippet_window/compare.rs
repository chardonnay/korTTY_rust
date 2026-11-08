//! Read-only comparison of the exact saved snippets selected in the library.

use std::collections::HashMap;
use std::fmt;
use std::ops::Range;
use std::sync::Arc;

use iced::widget::{self, button, column, container, row, scrollable, space, text};
use iced::{Alignment, Element, Font, Length, Task, Theme};

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
                    return Err("A snippet exceeds the 16,384-line comparison limit.");
                }
                let shown = if source_lines.peek().is_some() {
                    line.strip_suffix('\r').unwrap_or(line)
                } else {
                    line
                };
                if shown.len() > MAX_LINE_BYTES {
                    return Err("A snippet line exceeds the 4 KiB comparison limit.");
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
}

impl Default for View {
    fn default() -> Self {
        Self {
            comparison: None,
            page: 0,
            scroll: widget::Id::unique(),
        }
    }
}

impl View {
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
        let body: Element<'_, Message> = match &self.comparison {
            Some(Ok(diff)) => self.diff(diff),
            Some(Err(error)) => container(text(*error)).center(Length::Fill).into(),
            None => container(text("Comparing saved snippets…"))
                .center(Length::Fill)
                .into(),
        };
        column![
            body,
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

    fn diff<'a>(&'a self, diff: &'a Comparison) -> Element<'a, Message> {
        let pages = self.pages();
        let rows = diff
            .rows
            .iter()
            .skip(self.page * PAGE_ROWS)
            .take(PAGE_ROWS)
            .fold(column![].spacing(1), |rows, line| {
                rows.push(
                    row![
                        cell(&diff.left, line.left, line.changed, true),
                        cell(&diff.right, line.right, line.changed, false),
                    ]
                    .spacing(6),
                )
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

fn cell(file: &File, index: Option<usize>, changed: bool, left: bool) -> Element<'_, Message> {
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
            Some(if index.is_none() {
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
