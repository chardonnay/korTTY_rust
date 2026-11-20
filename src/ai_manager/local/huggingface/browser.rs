//! Sortable metadata for the Hub search results.

use std::cmp::Ordering;

use iced::Element;
use iced::widget::{button, column, scrollable, table, text};

use super::{Action, Format, GIB, Message, ModelListing, State};

const CELL_PADDING: u32 = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FormatFilter {
    All,
    Gguf,
    Mlx,
}

impl Default for FormatFilter {
    fn default() -> Self {
        if crate::local_ai::mlx_runtime::supported() {
            Self::All
        } else {
            Self::Gguf
        }
    }
}

impl std::fmt::Display for FormatFilter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::All => "All formats",
            Self::Gguf => "GGUF",
            Self::Mlx => "MLX",
        })
    }
}

impl FormatFilter {
    pub(super) const ALL: [Self; 3] = [Self::All, Self::Gguf, Self::Mlx];

    pub(super) fn formats(self) -> &'static [Format] {
        match self {
            Self::All => &[Format::Gguf, Format::Mlx],
            Self::Gguf => &[Format::Gguf],
            Self::Mlx => &[Format::Mlx],
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SortColumn {
    Model,
    Format,
    Architecture,
    Quantizations,
    License,
    Size,
    Context,
    Age,
    Memory,
}

impl SortColumn {
    const ALL: [Self; 9] = [
        Self::Model,
        Self::Format,
        Self::Architecture,
        Self::Quantizations,
        Self::License,
        Self::Size,
        Self::Context,
        Self::Age,
        Self::Memory,
    ];

    fn layout(self) -> (&'static str, u32) {
        match self {
            Self::Model => ("Model", 240),
            Self::Format => ("Format", 85),
            Self::Architecture => ("Architecture", 145),
            Self::Quantizations => ("Quantizations", 155),
            Self::License => ("License", 105),
            Self::Size => ("Size", 94),
            Self::Context => ("Context", 86),
            Self::Age => ("Age", 82),
            Self::Memory => ("RAM fit", 110),
        }
    }
}

#[derive(Default)]
pub(super) struct Sort {
    column: Option<SortColumn>,
    descending: bool,
}

impl Sort {
    pub(super) fn select(&mut self, column: SortColumn) {
        self.descending = self.column == Some(column) && !self.descending;
        self.column = Some(column);
    }

    fn order(&self, rows: &mut [ModelRow<'_>]) {
        let Some(column) = self.column else {
            return;
        };
        rows.sort_by(|left, right| {
            let order = match column {
                SortColumn::Model => self.compare(
                    Some(left.model.repository.id().to_ascii_lowercase()),
                    Some(right.model.repository.id().to_ascii_lowercase()),
                ),
                SortColumn::Format => {
                    self.compare(Some(left.model.format), Some(right.model.format))
                }
                SortColumn::Architecture => self.compare(
                    left.model
                        .architecture
                        .as_ref()
                        .map(|label| label.to_ascii_lowercase()),
                    right
                        .model
                        .architecture
                        .as_ref()
                        .map(|label| label.to_ascii_lowercase()),
                ),
                SortColumn::Quantizations => self.compare(
                    (!left.model.quantizations.is_empty()).then_some(&left.model.quantizations),
                    (!right.model.quantizations.is_empty()).then_some(&right.model.quantizations),
                ),
                SortColumn::License => self.compare(
                    left.model
                        .license
                        .as_ref()
                        .map(|label| label.to_ascii_lowercase()),
                    right
                        .model
                        .license
                        .as_ref()
                        .map(|label| label.to_ascii_lowercase()),
                ),
                SortColumn::Size => self.compare(left.bytes, right.bytes),
                SortColumn::Context => self.compare(left.model.context, right.model.context),
                SortColumn::Age => self.compare(left.age_days, right.age_days),
                SortColumn::Memory => self.compare(left.memory, right.memory),
            };
            order.then_with(|| left.model.repository.id().cmp(right.model.repository.id()))
        });
    }

    fn compare<T: Ord>(&self, left: Option<T>, right: Option<T>) -> Ordering {
        match (left, right) {
            (Some(left), Some(right)) if self.descending => right.cmp(&left),
            (Some(left), Some(right)) => left.cmp(&right),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum RamFit {
    TooLarge,
    Possible,
    Comfortable,
}

impl RamFit {
    fn estimate(bytes: u64, memory: u64) -> Self {
        let overhead = ((u128::from(bytes) * 35 + 50) / 100) as u64;
        let working_set = bytes.saturating_add(overhead.max(2 * GIB));
        if memory >= working_set.saturating_add(2 * GIB) {
            Self::Comfortable
        } else if memory >= working_set {
            Self::Possible
        } else {
            Self::TooLarge
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::TooLarge => "Too large",
            Self::Possible => "Possible",
            Self::Comfortable => "Comfortable",
        }
    }
}

#[derive(Clone, Copy)]
struct ModelRow<'a> {
    model: &'a ModelListing,
    bytes: Option<u64>,
    age_days: Option<i64>,
    memory: Option<RamFit>,
}

impl State {
    fn shows_model(&self, model: &ModelListing) -> bool {
        self.show_all_models
            || model.bytes.is_some_and(|bytes| {
                self.recommendations.memory.is_none_or(|memory| {
                    memory == 0 || RamFit::estimate(bytes, memory) != RamFit::TooLarge
                })
            })
    }

    pub(super) fn browser_view(&self, enabled: bool) -> Element<'_, Message> {
        let now = jiff::Timestamp::now();
        let reviewed_file = self.review.as_ref().zip(self.file.as_ref());
        let mut rows: Vec<_> = self
            .repositories
            .iter()
            .filter(|model| self.shows_model(model))
            .map(|model| {
                let bytes = reviewed_file
                    .filter(|(review, _)| review.repository == model.repository)
                    .map(|(_, file)| file.bytes())
                    .or(model.bytes);
                ModelRow {
                    model,
                    bytes,
                    age_days: model
                        .created
                        .filter(|&date| date <= now)
                        .map(|date| now.duration_since(date).as_secs() / 86_400),
                    memory: bytes
                        .zip(self.recommendations.memory.filter(|&bytes| bytes > 0))
                        .map(|(bytes, memory)| RamFit::estimate(bytes, memory)),
                }
            })
            .collect();
        let count = rows.len();
        let hidden = self.repositories.len() - count;
        let checking = self.sizing.pending.len();
        let notice = if checking > 0 {
            format!("Showing {count} models · {hidden} filtered · Checking model sizes…")
        } else {
            format!("Showing {count} models · {hidden} filtered")
        };
        let selected_hidden = self
            .repository
            .as_ref()
            .filter(|selected| !rows.iter().any(|row| &row.model.repository == *selected));
        self.sort.order(&mut rows);
        let mlx_supported = crate::local_ai::mlx_runtime::supported();
        let displayed = SortColumn::ALL
            .into_iter()
            .filter(|&column| mlx_supported || column != SortColumn::Format);
        let columns = displayed.clone().map(|column| {
            let (label, width) = column.layout();
            let indicator = match self.sort.column {
                Some(active) if active == column && self.sort.descending => " ↓",
                Some(active) if active == column => " ↑",
                _ => "",
            };
            table::column(
                button(text(format!("{label}{indicator}")).size(13))
                    .style(button::secondary)
                    .on_press_maybe(enabled.then_some(Message::HuggingFace(Action::Sort(column)))),
                move |row: ModelRow<'_>| -> Element<'_, Message> {
                    if column == SortColumn::Model {
                        let chosen = self.repository.as_ref() == Some(&row.model.repository);
                        return button(
                            text(row.model.repository.id())
                                .size(13)
                                .wrapping(text::Wrapping::WordOrGlyph),
                        )
                        .style(if chosen {
                            button::primary
                        } else {
                            button::secondary
                        })
                        .on_press_maybe(enabled.then(|| {
                            Message::HuggingFace(Action::Repository(row.model.repository.clone()))
                        }))
                        .into();
                    }
                    let value = match column {
                        SortColumn::Model => unreachable!("model name is a button"),
                        SortColumn::Format => row.model.format.to_string(),
                        SortColumn::Architecture => {
                            row.model.architecture.as_deref().unwrap_or("—").into()
                        }
                        SortColumn::Quantizations if !row.model.quantizations.is_empty() => {
                            row.model.quantizations.join(", ")
                        }
                        SortColumn::Quantizations => "—".into(),
                        SortColumn::License => row.model.license.as_deref().unwrap_or("—").into(),
                        SortColumn::Size => row.bytes.map_or_else(
                            || "—".into(),
                            |bytes| format!("{:.2} GiB", bytes as f64 / GIB as f64),
                        ),
                        SortColumn::Context => row
                            .model
                            .context
                            .map_or_else(|| "—".into(), |context| context.to_string()),
                        SortColumn::Age => row
                            .age_days
                            .map_or_else(|| "—".into(), |days| format!("{days} d")),
                        SortColumn::Memory => row.memory.map_or("Unknown", RamFit::label).into(),
                    };
                    text(value)
                        .size(13)
                        .wrapping(text::Wrapping::WordOrGlyph)
                        .into()
                },
            )
            .width(width)
        });
        // Keep the table finite inside a horizontally scrolling viewport.
        let width = displayed
            .map(|column| column.layout().1 + 2 * CELL_PADDING + 1)
            .sum::<u32>();
        let grid = (count > 0).then(|| {
            scrollable(
                table(columns, rows)
                    .width(width)
                    .padding(CELL_PADDING as f32),
            )
            .direction(scrollable::Direction::Both {
                vertical: scrollable::Scrollbar::new(),
                horizontal: scrollable::Scrollbar::new(),
            })
            .height(236)
        });
        column![
            text(notice).size(13),
            selected_hidden
                .map(|repository| text(format!("Selected model · {repository}")).size(13)),
            grid,
        ]
        .spacing(6)
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_sort_reverses_known_values_and_leaves_missing_values_last() {
        let listing = |id: &str| ModelListing::for_test(serde_json::from_value(id.into()).unwrap());
        let first = listing("example/first");
        let second = listing("example/second");
        let third = listing("example/third");
        let row = |model, bytes| ModelRow {
            model,
            bytes,
            age_days: None,
            memory: None,
        };
        let mut rows = [
            row(&first, Some(20)),
            row(&third, None),
            row(&second, Some(3)),
        ];
        let mut sort = Sort::default();
        sort.select(SortColumn::Size);
        sort.order(&mut rows);
        assert_eq!(rows.map(|row| row.bytes), [Some(3), Some(20), None]);
        sort.select(SortColumn::Size);
        sort.order(&mut rows);
        assert_eq!(rows.map(|row| row.bytes), [Some(20), Some(3), None]);
        assert_eq!(RamFit::estimate(4 * GIB, 8 * GIB), RamFit::Comfortable);
        assert_eq!(RamFit::estimate(4 * GIB, 6 * GIB), RamFit::Possible);
        assert_eq!(RamFit::estimate(4 * GIB, 5 * GIB), RamFit::TooLarge);
    }

    #[test]
    fn filtering_uses_the_smallest_known_weights_and_retains_the_show_all_option() {
        let mut state = State::default();
        state.recommendations.memory = Some(8 * GIB);
        let repository = serde_json::from_value("example/fixture".into()).unwrap();
        let mut listing = ModelListing::for_test(repository);
        assert!(!state.shows_model(&listing));
        listing.bytes = Some(8 * GIB);
        assert!(!state.shows_model(&listing));
        listing.bytes = Some(4 * GIB);
        assert!(state.shows_model(&listing));
        listing.bytes = None;
        state.show_all_models = true;
        assert!(state.shows_model(&listing));
    }
}
