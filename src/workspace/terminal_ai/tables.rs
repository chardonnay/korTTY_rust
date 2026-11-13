//! Native rendering and clipboard actions for bounded Markdown pipe tables.

use std::ops::Range;
use std::sync::{Arc, Weak};

use iced::widget::{Column, Row, button, column, container, row, scrollable, text, tooltip};
use iced::{Element, Font, Length, Task, Theme, alignment, window};

use super::{Desktop, Message, Turn, Workspace, visual_blocks};

const MAX_COLUMNS: usize = 8;
const MAX_ROWS: usize = 24;
const MAX_CELL_BYTES: usize = 1_024;
const MAX_CELLS_PER_ANSWER: usize = 256;
const MAX_TABLES_PER_ANSWER: usize = 6;
const CELL_WIDTH: f32 = 168.0;

#[derive(Default)]
struct Budget {
    tables: usize,
    cells: usize,
}

impl Budget {
    fn allow(&mut self, table: &Table) -> bool {
        let cells = table.columns.len() * table.rows.len();
        if self.tables == MAX_TABLES_PER_ANSWER || self.cells + cells > MAX_CELLS_PER_ANSWER {
            return false;
        }
        self.tables += 1;
        self.cells += cells;
        true
    }
}

#[derive(Clone, Copy)]
enum Copy {
    Table,
    Column(usize),
    Cell { row: usize, column: usize },
}

#[derive(Clone)]
pub struct Selection {
    turn: Weak<Turn>,
    range: Range<usize>,
    copy: Copy,
}

impl Selection {
    fn text(&self, turns: &[Arc<Turn>]) -> Option<String> {
        let turn = self.turn.upgrade()?;
        if !turns.iter().any(|candidate| Arc::ptr_eq(candidate, &turn)) {
            return None;
        }
        let source = turn.answer().get(self.range.clone())?;
        let table = find(source, 0).next()?;
        if table.range != (0..source.len()) {
            return None;
        }
        table.copy(self.copy)
    }
}

impl Workspace {
    pub(super) fn copy_chat_table(&self, id: window::Id, selection: &Selection) -> Task<Desktop> {
        let Some(chat) = self
            .terminal_chats
            .get(&id)
            .filter(|chat| chat.code_actions_available() && chat.code_review.is_none())
        else {
            return Task::none();
        };
        if !self.ai_chat_enabled() || self.private_agent_source_changed(chat) {
            return Task::none();
        }
        selection
            .text(&chat.turns)
            .map_or_else(Task::none, |value| self.write_clipboard(value))
    }
}

#[derive(Clone, Copy)]
enum Align {
    Default,
    Left,
    Center,
    Right,
}

struct Cell {
    text: String,
    bold: bool,
}

impl Cell {
    fn new(value: &str) -> Self {
        let bold = value.len() >= 4 && value.starts_with("**") && value.ends_with("**");
        let text = value
            .replace("**", "")
            .replace('`', "")
            .replace(['\u{202f}', '\t'], " ");
        Self {
            text: text.trim().into(),
            bold,
        }
    }

    fn numeric(&self) -> bool {
        let value = self
            .text
            .replace(['%', ' ', '(', ')'], "")
            .replace(',', ".");
        let value = value.strip_prefix(['+', '-']).unwrap_or(&value);
        let digits =
            |value: &str| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit());
        value.split_once('.').map_or_else(
            || digits(value),
            |(integer, fraction)| digits(integer) && digits(fraction),
        )
    }
}

struct Table {
    range: Range<usize>,
    columns: Vec<Align>,
    rows: Vec<Vec<Cell>>,
}

impl Table {
    fn copy(&self, copy: Copy) -> Option<String> {
        match copy {
            Copy::Table => Some(
                self.rows
                    .iter()
                    .map(|row| {
                        row.iter()
                            .map(|cell| cell.text.as_str())
                            .collect::<Vec<_>>()
                            .join("\t")
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            Copy::Column(column) if column < self.columns.len() => Some(
                self.rows
                    .iter()
                    .map(|row| row[column].text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            Copy::Cell { row, column } => Some(self.rows.get(row)?.get(column)?.text.clone()),
            _ => None,
        }
    }

    fn view(self, turn: &Arc<Turn>, font_size: f32, enabled: bool) -> Element<'static, Message> {
        let select = |copy| {
            enabled.then(|| {
                Message::CopyTable(Selection {
                    turn: Arc::downgrade(turn),
                    range: self.range.clone(),
                    copy,
                })
            })
        };
        let width = CELL_WIDTH.max(font_size * 10.0);
        let mut table = column![].spacing(1);
        for (row_index, cells) in self.rows.into_iter().enumerate() {
            let header = row_index == 0;
            let mut line = row![].spacing(1);
            for (column, cell) in cells.into_iter().enumerate() {
                let alignment = match self.columns[column] {
                    Align::Right => alignment::Horizontal::Right,
                    Align::Center => alignment::Horizontal::Center,
                    Align::Default if !header && cell.numeric() => alignment::Horizontal::Right,
                    _ => alignment::Horizontal::Left,
                };
                let value = if cell.text.is_empty() {
                    " ".into()
                } else {
                    cell.text
                };
                let value = text(value)
                    .size(font_size)
                    .width(Length::Fill)
                    .align_x(alignment)
                    .font(if header || cell.bold {
                        Font {
                            weight: iced::font::Weight::Bold,
                            ..Font::DEFAULT
                        }
                    } else {
                        Font::DEFAULT
                    })
                    .wrapping(text::Wrapping::WordOrGlyph);
                let mut contents = Row::new().align_y(alignment::Vertical::Center);
                if header {
                    contents = contents.push(value).spacing(6).push(tooltip(
                        button("⧉")
                            .style(|theme, status| copy_style(theme, status, true))
                            .padding([1, 4])
                            .on_press_maybe(select(Copy::Column(column))),
                        text("Copy column").size(13),
                        tooltip::Position::Top,
                    ));
                } else {
                    contents = contents.push(tooltip(
                        button(value)
                            .width(Length::Fill)
                            .style(|theme, status| copy_style(theme, status, false))
                            .padding(0)
                            .on_press_maybe(select(Copy::Cell {
                                row: row_index,
                                column,
                            })),
                        text("Copy cell").size(13),
                        tooltip::Position::Top,
                    ));
                }
                line = line.push(container(contents).width(width).padding([6, 8]).style(
                    move |theme: &Theme| {
                        let (background, text) = cell_color(theme, header);
                        container::Style {
                            background: Some(background.into()),
                            text_color: Some(text),
                            ..Default::default()
                        }
                    },
                ));
            }
            table = table.push(line);
        }
        container(
            column![
                button("Copy table")
                    .style(button::secondary)
                    .on_press_maybe(select(Copy::Table)),
                scrollable(table)
                    .direction(scrollable::Direction::Horizontal(
                        scrollable::Scrollbar::new()
                    ))
                    .width(Length::Fill),
            ]
            .spacing(6),
        )
        .style(container::bordered_box)
        .padding(6)
        .width(Length::Fill)
        .into()
    }
}

fn cell_color(theme: &Theme, header: bool) -> (iced::Color, iced::Color) {
    let palette = theme.extended_palette();
    let pair = if header {
        palette.primary.weak
    } else {
        palette.background.weak
    };
    (pair.color, pair.text)
}

fn copy_style(theme: &Theme, status: button::Status, header: bool) -> button::Style {
    let mut style = button::text(theme, status);
    let color = cell_color(theme, header).1;
    style.text_color = if matches!(status, button::Status::Hovered) {
        color.scale_alpha(0.8)
    } else {
        color
    };
    style
}

struct Prose<'a> {
    lines: std::iter::Peekable<Lines<'a>>,
}

struct Lines<'a> {
    lines: std::str::SplitInclusive<'a, char>,
    offset: usize,
}

impl<'a> Iterator for Lines<'a> {
    type Item = (usize, &'a str);

    fn next(&mut self) -> Option<Self::Item> {
        let line = self.lines.next()?;
        let start = self.offset;
        self.offset += line.len();
        Some((start, line))
    }
}

impl Iterator for Prose<'_> {
    type Item = Table;

    fn next(&mut self) -> Option<Table> {
        while let Some((start, header)) = self.lines.next() {
            if !has_pipe(header) {
                continue;
            }
            let Some(&(separator_start, separator)) = self.lines.peek() else {
                break;
            };
            let Some(columns) = parse_row(separator).and_then(|cells| {
                cells
                    .iter()
                    .map(|cell| delimiter(cell))
                    .collect::<Option<Vec<_>>>()
            }) else {
                continue;
            };
            self.lines.next();
            let mut end = separator_start + separator.len();
            let mut valid = true;
            let mut rows = Vec::new();
            for line in std::iter::once(header).chain(std::iter::from_fn(|| {
                let (start, line) = self.lines.next_if(|(_, line)| has_pipe(line))?;
                end = start + line.len();
                Some(line)
            })) {
                if valid
                    && rows.len() <= MAX_ROWS
                    && !line.contains("data:image/")
                    && let Some(cells) =
                        parse_row(line).filter(|cells| cells.len() == columns.len())
                {
                    rows.push(cells.iter().map(|cell| Cell::new(cell)).collect());
                } else {
                    valid = false;
                }
            }
            if valid && rows.len() > 1 {
                return Some(Table {
                    range: start..end,
                    columns,
                    rows,
                });
            }
        }
        None
    }
}

fn find(source: &str, offset: usize) -> Prose<'_> {
    Prose {
        lines: Lines {
            lines: source.split_inclusive('\n'),
            offset,
        }
        .peekable(),
    }
}

fn markdown_line(line: &str) -> Option<&str> {
    let content = line.trim_start_matches(' ');
    (line.len() - content.len() <= 3 && !content.starts_with('\t')).then(|| content.trim_end())
}

fn pipe_offsets(line: &str) -> impl Iterator<Item = usize> + '_ {
    let mut escaped = false;
    line.bytes().enumerate().filter_map(move |(offset, byte)| {
        let pipe = byte == b'|' && !escaped;
        escaped = byte == b'\\' && !escaped;
        pipe.then_some(offset)
    })
}

fn has_pipe(line: &str) -> bool {
    markdown_line(line).is_some_and(|line| pipe_offsets(line).next().is_some())
}

fn parse_row(line: &str) -> Option<Vec<String>> {
    let line = markdown_line(line)?;
    let mut offsets = pipe_offsets(line);
    let first = offsets.next()?;
    let mut cells = Vec::new();
    let mut start = 0;
    for separator in std::iter::once(first).chain(offsets) {
        if separator != 0 {
            push_cell(&mut cells, &line[start..separator])?;
        }
        start = separator + 1;
    }
    if start < line.len() {
        push_cell(&mut cells, &line[start..])?;
    }
    (!cells.is_empty()).then_some(cells)
}

fn push_cell(cells: &mut Vec<String>, value: &str) -> Option<()> {
    let value = value.trim();
    if cells.len() == MAX_COLUMNS || value.len() > MAX_CELL_BYTES {
        return None;
    }
    let mut cell = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\\' && matches!(chars.peek(), Some('|' | '\\')) {
            cell.push(chars.next().expect("peeked escaped character"));
        } else {
            cell.push(character);
        }
    }
    cells.push(cell);
    Some(())
}

fn delimiter(value: &str) -> Option<Align> {
    let left = value.starts_with(':');
    let right = value.ends_with(':');
    let hyphens = value.strip_prefix(':').unwrap_or(value);
    let hyphens = hyphens.strip_suffix(':').unwrap_or(hyphens);
    if hyphens.is_empty() || !hyphens.bytes().all(|byte| byte == b'-') {
        return None;
    }
    Some(match (left, right) {
        (false, false) => Align::Default,
        (true, false) => Align::Left,
        (false, true) => Align::Right,
        (true, true) => Align::Center,
    })
}

pub(super) struct ProseView<'a, 'visuals> {
    visuals: &'visuals visual_blocks::State,
    turn: &'a Arc<Turn>,
    font_size: f32,
    enabled: bool,
    inline_budget: usize,
    table_budget: Budget,
}

impl<'a, 'visuals> ProseView<'a, 'visuals> {
    pub(super) fn new(
        visuals: &'visuals visual_blocks::State,
        turn: &'a Arc<Turn>,
        font_size: f32,
        enabled: bool,
    ) -> Self {
        Self {
            visuals,
            turn,
            font_size,
            enabled,
            inline_budget: visual_blocks::MAX_INLINE_SOURCES,
            table_budget: Budget::default(),
        }
    }

    pub(super) fn append(
        &mut self,
        mut content: Column<'a, Message>,
        range: Range<usize>,
    ) -> Column<'a, Message> {
        let mut previous = range.start;
        for table in find(&self.turn.answer()[range.clone()], range.start) {
            if !self.table_budget.allow(&table) {
                continue;
            }
            content = self.text(content, previous..table.range.start);
            previous = table.range.end;
            content = content.push(table.view(self.turn, self.font_size, self.enabled));
        }
        self.text(content, previous..range.end)
    }

    fn text(&mut self, content: Column<'a, Message>, range: Range<usize>) -> Column<'a, Message> {
        self.visuals.append_text(
            content,
            self.turn,
            range,
            self.font_size,
            &mut self.inline_budget,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str =
        "| **Name** | `Value` |\r\n| :--- | ---: |\r\n| α\\|β | **1,5%** |\r\n| Empty | |\r\n";

    #[test]
    fn markdown_table_copies_the_displayed_tsv_column_and_cell() {
        let source = format!("Before\r\n\r\n{TABLE}\r\nAfter");
        let table = find(&source, 0).next().unwrap();
        assert_eq!(&source[table.range.clone()], TABLE);
        assert!(matches!(table.columns[..], [Align::Left, Align::Right]));
        assert!(table.rows[1][1].numeric());
        assert!(table.rows[1][1].bold);
        assert_eq!(
            table.copy(Copy::Table).as_deref(),
            Some("Name\tValue\nα|β\t1,5%\nEmpty\t")
        );
        assert_eq!(
            table.copy(Copy::Column(1)).as_deref(),
            Some("Value\n1,5%\n")
        );
        assert_eq!(
            table.copy(Copy::Cell { row: 1, column: 0 }).as_deref(),
            Some("α|β")
        );
        assert!(table.copy(Copy::Cell { row: 20, column: 0 }).is_none());
        assert!(table.copy(Copy::Column(20)).is_none());
    }

    #[test]
    fn malformed_or_oversized_table_is_left_as_prose() {
        for source in [
            "| One | Two |\n| - |\n| first | second |\n".to_owned(),
            "| One | Two |\n| - | - |\n| first |\n| third | fourth |\n".to_owned(),
            "| One | Two |\n| : | --- |\n| first | second |\n".to_owned(),
            "    | One | Two |\n    | --- | --- |\n    | first | second |\n".to_owned(),
            "| One | Two |\n| - | - |\n| first | ![](data:image/png;base64,AAAA) |\n".to_owned(),
            format!(
                "| One | Two |\n| - | - |\n{}",
                "| a | b |\n".repeat(MAX_ROWS + 1)
            ),
            format!(
                "| One | Two |\n| - | - |\n| {} | b |\n",
                "x".repeat(MAX_CELL_BYTES + 1)
            ),
        ] {
            assert!(find(&source, 0).next().is_none());
        }
        let table = find("Name | Value\n:---: | ---\nleft | right", 0)
            .next()
            .unwrap();
        assert!(matches!(table.columns[..], [Align::Center, Align::Default]));
        let mut budget = Budget::default();
        assert!((0..MAX_TABLES_PER_ANSWER).all(|_| budget.allow(&table)));
        assert!(!budget.allow(&table));
    }

    #[test]
    fn table_clipboard_action_rejects_replaced_turns_and_invalid_ranges() {
        let turn = Arc::new(Turn::from_parts(None, "table".into(), TABLE.into()));
        let selection = Selection {
            turn: Arc::downgrade(&turn),
            range: 0..TABLE.len(),
            copy: Copy::Column(0),
        };
        let other = Arc::new(Turn::from_parts(None, "table".into(), TABLE.into()));
        assert_eq!(
            selection.text(std::slice::from_ref(&turn)).as_deref(),
            Some("Name\nα|β\nEmpty")
        );
        assert!(selection.text(&[other]).is_none());
        let mut forged = selection.clone();
        forged.range = 0..TABLE.len() + 1;
        assert!(forged.text(std::slice::from_ref(&turn)).is_none());
        forged.range = 0..TABLE.find("| Empty").unwrap() + 1;
        assert!(forged.text(&[turn]).is_none());
        assert!(selection.text(&[]).is_none());
    }

    #[test]
    fn selected_table_routes_through_workspace_clipboard_and_rejects_stale_turn() {
        use super::super::{Action, Chat, Configuration, Redactor, Secrets};

        let configuration = serde_json::from_value::<Configuration>(serde_json::json!({
            "profiles": { "profiles": [{ "id": 1, "name": "Local", "endpoint": "127.0.0.1:12345", "model": "test" }], "default": 1, "last_id": 1 }
        })).unwrap();
        let mut workspace = Workspace::default();
        workspace.set_test_ai_configuration(configuration.clone());
        workspace.clipboard.policy.internal = true;
        let id = window::Id::unique();
        let redactor =
            Redactor::for_journal(Secrets::default().seal().unwrap(), Default::default());
        let mut chat = Chat::new(&configuration, Action::Ask, "", redactor, None).unwrap();
        let turn = Arc::new(Turn::from_parts(None, "table".into(), TABLE.into()));
        chat.turns.push(Arc::clone(&turn));
        workspace.terminal_chats.insert(id, chat);
        let select = |copy| {
            Message::CopyTable(Selection {
                turn: Arc::downgrade(&turn),
                range: 0..TABLE.len(),
                copy,
            })
        };
        for (copy, expected) in [
            (Copy::Table, "Name\tValue\nα|β\t1,5%\nEmpty\t"),
            (Copy::Column(1), "Value\n1,5%\n"),
            (Copy::Cell { row: 1, column: 0 }, "α|β"),
        ] {
            let _ = workspace.update_terminal_ai(id, select(copy));
            assert_eq!(workspace.clipboard.buffer.get().as_deref(), Some(expected));
        }
        workspace.terminal_chats.get_mut(&id).unwrap().turns.clear();
        let _ = workspace.update_terminal_ai(id, select(Copy::Table));
        assert_eq!(workspace.clipboard.buffer.get().as_deref(), Some("α|β"));
    }
}
