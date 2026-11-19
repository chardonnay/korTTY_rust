//! Column layout for snippet metadata.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use iced::advanced::text::Wrapping;
use iced::widget::{Row, button, canvas, container, row, space, text, tooltip};
use iced::{Alignment, Element, Length};
use serde::{Deserialize, Serialize};

use crate::horizontal_divider::{self, Grow};
use crate::snippet_analysis::store::overview::{Kind, Overview, Status};
use crate::snippets::{Snippet, SnippetId};

use super::Message;

pub(super) const SELECT_WIDTH: f32 = 24.0;
const HEADER_HEIGHT: f32 = 26.0;
const MAX_WIDTH: u16 = 900;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Column {
    Name,
    Language,
    Category,
    OperatingSystem,
    Tags,
    Executable,
    Favorite,
    LineCount,
    LastModified,
    UsageCount,
    AnalysisStatus,
}

impl Column {
    const ALL: [Self; 11] = [
        Self::Name,
        Self::Language,
        Self::Category,
        Self::OperatingSystem,
        Self::Tags,
        Self::Executable,
        Self::Favorite,
        Self::LineCount,
        Self::LastModified,
        Self::UsageCount,
        Self::AnalysisStatus,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Language => "Language",
            Self::Category => "Category",
            Self::OperatingSystem => "Operating system",
            Self::Tags => "Tags",
            Self::Executable => "Executable",
            Self::Favorite => "Favorite",
            Self::LineCount => "Lines",
            Self::LastModified => "Modified (UTC)",
            Self::UsageCount => "Uses",
            Self::AnalysisStatus => "Analysis status",
        }
    }

    fn defaults(self) -> (u16, u16) {
        match self {
            Self::Name => (78, 150),
            Self::Language => (78, 90),
            Self::Category => (78, 100),
            Self::OperatingSystem => (118, 125),
            Self::Tags => (64, 160),
            Self::Executable => (84, 84),
            Self::Favorite => (32, 68),
            Self::LineCount => (48, 56),
            Self::LastModified => (104, 142),
            Self::UsageCount => (52, 75),
            Self::AnalysisStatus => (72, 160),
        }
    }

    pub(super) fn fit(self, width: f64) -> Option<u16> {
        (width.is_finite() && width > 0.0).then(|| {
            width
                .round()
                .clamp(f64::from(self.defaults().0), f64::from(MAX_WIDTH)) as u16
        })
    }

    fn java_id(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Language => "language",
            Self::Category => "category",
            Self::OperatingSystem => "operatingSystem",
            Self::Tags => "tags",
            Self::Executable => "executable",
            Self::Favorite => "favorite",
            Self::LineCount => "lineCount",
            Self::LastModified => "lastModified",
            Self::UsageCount => "usageCount",
            Self::AnalysisStatus => "analysisStatus",
        }
    }

    fn sort_key(self, snippet: &Snippet, overview: &Overview) -> SortKey {
        let metadata = snippet.metadata();
        match self {
            Self::Name => SortKey::Text(snippet.name().to_lowercase()),
            Self::Language => SortKey::Text(snippet.language().to_lowercase()),
            Self::Category => SortKey::Text(snippet.category().to_lowercase()),
            Self::OperatingSystem => SortKey::Text(
                metadata
                    .operating_system
                    .as_deref()
                    .unwrap_or_default()
                    .to_lowercase(),
            ),
            Self::Tags => SortKey::Text(metadata.tags.join(", ").to_lowercase()),
            Self::Executable => {
                SortKey::Flag(crate::snippets::archive::snippet_is_executable(snippet))
            }
            Self::Favorite => SortKey::Flag(snippet.favorite()),
            Self::LineCount => SortKey::Count(line_count(snippet.content())),
            Self::LastModified => SortKey::Timestamp(snippet.last_modified()),
            Self::UsageCount => SortKey::Count(snippet.usage_count() as usize),
            Self::AnalysisStatus => SortKey::Count(overview.status(snippet).sort_rank()),
        }
    }
}

#[derive(Eq, Ord, PartialEq, PartialOrd)]
enum SortKey {
    Text(String),
    Flag(bool),
    Count(usize),
    Timestamp(Option<jiff::Timestamp>),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum Sort {
    #[default]
    Default,
    Ascending(Column),
    Descending(Column),
}

impl Sort {
    pub(super) fn next(self, column: Column) -> Self {
        match self {
            Self::Ascending(current) if current == column => Self::Descending(column),
            Self::Descending(current) if current == column => Self::Default,
            _ => Self::Ascending(column),
        }
    }

    fn label(self, column: Column) -> String {
        let direction = if self == Self::Ascending(column) {
            " ↑"
        } else if self == Self::Descending(column) {
            " ↓"
        } else {
            ""
        };
        format!("{}{direction}", column.title())
    }

    pub(super) fn apply(
        self,
        rows: &mut [usize],
        entries: &[Snippet],
        previous_order: &HashMap<SnippetId, usize>,
        overview: &Overview,
    ) {
        match self {
            Self::Default => rows.sort_by_cached_key(|&index| {
                let snippet = &entries[index];
                (
                    previous_order
                        .get(&snippet.id())
                        .copied()
                        .unwrap_or(usize::MAX),
                    Reverse(snippet.favorite()),
                    Reverse(snippet.usage_count()),
                    snippet.name().to_lowercase(),
                )
            }),
            Self::Ascending(column) => {
                rows.sort_by_cached_key(|&index| column.sort_key(&entries[index], overview))
            }
            Self::Descending(column) => rows
                .sort_by_cached_key(|&index| Reverse(column.sort_key(&entries[index], overview))),
        }
    }

    pub(super) fn sorts_analysis(self) -> bool {
        matches!(
            self,
            Self::Ascending(Column::AnalysisStatus) | Self::Descending(Column::AnalysisStatus)
        )
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub(crate) struct Widths(BTreeMap<Column, u16>);

impl Widths {
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn valid(&self) -> bool {
        self.0
            .iter()
            .all(|(column, width)| (column.defaults().0..=MAX_WIDTH).contains(width))
    }

    pub(crate) fn save(&mut self, column: Column, width: u16) -> bool {
        let explicit = self.0.get(&column).copied();
        if column.fit(f64::from(width)) != Some(width) || explicit == Some(width) {
            return false;
        }
        self.0.insert(column, width);
        true
    }

    pub(crate) fn read_java(text: &str, omitted: &mut usize) -> Self {
        let mut seen = BTreeSet::new();
        let mut widths = BTreeMap::new();
        for part in text
            .split(';')
            .map(str::trim)
            .filter(|part| !part.is_empty())
        {
            let value = part.split_once('=').and_then(|(id, width)| {
                let column = Column::ALL
                    .into_iter()
                    .find(|column| column.java_id() == id.trim())?;
                Some((
                    column,
                    width
                        .trim()
                        .parse()
                        .ok()
                        .and_then(|width| column.fit(width)),
                ))
            });
            match value {
                Some((column, Some(width))) if seen.insert(column) => {
                    widths.insert(column, width);
                }
                Some((column, _)) => {
                    seen.insert(column);
                    widths.remove(&column);
                    *omitted += 1;
                }
                None => *omitted += 1,
            }
        }
        Self(widths)
    }

    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }

    fn width(&self, column: Column, drag: Option<(Column, u16)>) -> f32 {
        let width = drag
            .filter(|(target, _)| *target == column)
            .map(|(_, width)| width)
            .or_else(|| self.0.get(&column).copied())
            .unwrap_or_else(|| column.defaults().1);
        f32::from(width)
    }
}

pub(super) fn header<'a>(
    widths: &Widths,
    drag: Option<(Column, u16)>,
    sort: Sort,
) -> Row<'a, Message> {
    Column::ALL.into_iter().fold(
        row![space::horizontal().width(SELECT_WIDTH)],
        |headers, column| {
            let width = widths.width(column, drag);
            let grip = canvas(horizontal_divider::program(
                width,
                Grow::Right,
                move |value| Message::ResizeLibraryColumn(column, value),
                Some(Message::FinishLibraryColumnResize(column)),
            ))
            .width(horizontal_divider::WIDTH)
            .height(HEADER_HEIGHT);
            headers.push(
                row![
                    button(text(sort.label(column)).size(13).wrapping(Wrapping::None))
                        .padding([0, 6])
                        .width(width - horizontal_divider::WIDTH)
                        .height(HEADER_HEIGHT)
                        .clip(true)
                        .style(button::text)
                        .on_press(Message::SortLibrary(column)),
                    grip,
                ]
                .align_y(Alignment::Center),
            )
        },
    )
}

pub(super) fn cells<'a>(
    snippet: &'a Snippet,
    line_count: usize,
    analysis: Status,
    widths: &Widths,
    drag: Option<(Column, u16)>,
) -> Row<'a, Message> {
    let metadata = snippet.metadata();
    Column::ALL.into_iter().fold(row![], |cells, column| {
        let value = match column {
            Column::Name => text(snippet.name()),
            Column::Language => text(snippet.language()),
            Column::Category => text(snippet.category()),
            Column::OperatingSystem => {
                text(metadata.operating_system.as_deref().unwrap_or_default())
            }
            Column::Tags => text(metadata.tags.join(", ")),
            Column::Executable => text(
                if crate::snippets::archive::snippet_is_executable(snippet) {
                    "Yes"
                } else {
                    "No"
                },
            ),
            Column::Favorite => {
                return cells.push(cell(favorite_button(snippet), widths.width(column, drag)));
            }
            Column::LineCount => text(line_count.to_string()),
            Column::UsageCount => text(snippet.usage_count().to_string()),
            Column::AnalysisStatus => {
                let width = widths.width(column, drag);
                let label = analysis_label(analysis);
                let display = text(label.clone()).size(13).wrapping(Wrapping::None);
                if analysis.kind == Kind::None {
                    return cells.push(cell(display.into(), width));
                }
                return cells.push(cell(
                    tooltip(display, text(label).size(13), tooltip::Position::Top).into(),
                    width,
                ));
            }
            Column::LastModified => {
                text(snippet.last_modified().map_or_else(String::new, |stamp| {
                    stamp
                        .to_zoned(jiff::tz::TimeZone::UTC)
                        .strftime("%Y-%m-%d %H:%M")
                        .to_string()
                }))
            }
        };
        cells.push(cell(
            value.size(13).wrapping(Wrapping::None).into(),
            widths.width(column, drag),
        ))
    })
}

fn analysis_label(status: Status) -> String {
    let mut label = match status.kind {
        Kind::Review => "Review pending".into(),
        Kind::Findings => format!("{} open", status.open),
        Kind::Applied => "Applied".into(),
        Kind::Clean => "Clean".into(),
        Kind::None => String::new(),
        Kind::Unavailable => "Unavailable".into(),
    };
    if status.stale {
        label.push_str(" · Stale");
    }
    if status.unsaved {
        label.push_str(" · Unsaved result");
    }
    label
}

fn cell(content: Element<'_, Message>, width: f32) -> Element<'_, Message> {
    container(content)
        .width(Length::Fixed(width))
        .height(32)
        .align_y(Alignment::Center)
        .padding([0, 6])
        .clip(true)
        .into()
}

fn favorite_button(snippet: &Snippet) -> Element<'_, Message> {
    let (star, label) = if snippet.favorite() {
        ("★", "Remove from favorites")
    } else {
        ("☆", "Add to favorites")
    };
    tooltip(
        button(text(star).size(14))
            .padding([2, 4])
            .on_press(Message::ToggleFavorite(snippet.id())),
        label,
        tooltip::Position::Top,
    )
    .into()
}

pub(super) fn line_count(content: &str) -> usize {
    let mut bytes = content.bytes().peekable();
    let mut count = 0;
    while let Some(byte) = bytes.next() {
        match byte {
            b'\r' => {
                bytes.next_if_eq(&b'\n');
                count += 1;
            }
            b'\n' => count += 1,
            _ => {}
        }
    }
    count
        + usize::from(
            content
                .as_bytes()
                .last()
                .is_some_and(|byte| !matches!(byte, b'\r' | b'\n')),
        )
}

#[cfg(test)]
mod tests {
    use super::line_count;

    #[test]
    fn counts_java_line_endings_without_an_extra_trailing_line() {
        for (content, expected) in [
            ("", 0),
            ("東京", 1),
            ("\n", 1),
            ("\r\n", 1),
            ("first\r\nsecond\rthird\n", 3),
            ("first\r\n\nlast", 3),
            ("first\u{2028}second", 1),
        ] {
            assert_eq!(line_count(content), expected);
        }
    }
}
