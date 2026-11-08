use std::collections::BTreeSet;
use std::fmt;

const MAX_QUERY_BYTES: usize = 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum JournalSearchMode {
    #[default]
    AllFields,
    SelectedColumns,
}

impl JournalSearchMode {
    pub(super) const ALL: [Self; 2] = [Self::AllFields, Self::SelectedColumns];
}

impl fmt::Display for JournalSearchMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::AllFields => "All fields",
            Self::SelectedColumns => "Selected columns",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum JournalColumn {
    Started,
    Finished,
    Status,
    Job,
    Summary,
    Detail,
    Trigger,
    RunId,
    ExitCode,
}

impl JournalColumn {
    pub(super) const ALL: [Self; 9] = [
        Self::Started,
        Self::Finished,
        Self::Status,
        Self::Job,
        Self::Summary,
        Self::Detail,
        Self::Trigger,
        Self::RunId,
        Self::ExitCode,
    ];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Started => "Started",
            Self::Finished => "Finished",
            Self::Status => "Status",
            Self::Job => "Job",
            Self::Summary => "Summary",
            Self::Detail => "Detail",
            Self::Trigger => "Trigger",
            Self::RunId => "Run ID",
            Self::ExitCode => "Exit code",
        }
    }

    const fn bit(self) -> u16 {
        1 << self as u8
    }
}

pub(super) struct Search {
    text: String,
    terms: BTreeSet<String>,
    mode: JournalSearchMode,
    columns: u16,
    expanded: bool,
}

impl Default for Search {
    fn default() -> Self {
        Self {
            text: String::new(),
            terms: BTreeSet::new(),
            mode: JournalSearchMode::AllFields,
            columns: JournalColumn::Started.bit()
                | JournalColumn::Status.bit()
                | JournalColumn::Job.bit()
                | JournalColumn::Summary.bit(),
            expanded: false,
        }
    }
}

impl Search {
    pub(super) fn text(&self) -> &str {
        &self.text
    }

    pub(super) fn set_text(&mut self, text: String) {
        if text.len() <= MAX_QUERY_BYTES {
            self.terms = text.split_whitespace().map(str::to_lowercase).collect();
            self.text = text;
        }
    }

    pub(super) fn mode(&self) -> JournalSearchMode {
        self.mode
    }

    pub(super) fn set_mode(&mut self, mode: JournalSearchMode) {
        self.mode = mode;
        self.expanded = false;
    }

    pub(super) fn expanded(&self) -> bool {
        self.expanded
    }

    pub(super) fn collapse(&mut self) {
        self.expanded = false;
    }

    pub(super) fn toggle_columns(&mut self) {
        if self.mode == JournalSearchMode::SelectedColumns {
            self.expanded = !self.expanded;
        }
    }

    pub(super) fn includes(&self, column: JournalColumn) -> bool {
        self.columns & column.bit() != 0
    }

    pub(super) fn set_column(&mut self, column: JournalColumn, enabled: bool) {
        if self.expanded {
            if enabled {
                self.columns |= column.bit();
            } else {
                self.columns &= !column.bit();
            }
        }
    }

    pub(super) fn column_count(&self) -> u32 {
        self.columns.count_ones()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    pub(super) fn searches(&self, column: JournalColumn) -> bool {
        self.mode == JournalSearchMode::AllFields || self.includes(column)
    }

    pub(super) fn matches<'a>(
        &self,
        fields: impl IntoIterator<Item = (Option<JournalColumn>, &'a str)>,
    ) -> bool {
        if self.is_empty() {
            return true;
        }
        let mut unmatched = self.terms.iter().collect::<Vec<_>>();
        for (column, value) in fields {
            if self.mode == JournalSearchMode::SelectedColumns
                && column.is_none_or(|column| !self.includes(column))
            {
                continue;
            }
            let value = value.to_lowercase();
            unmatched.retain(|term| {
                term.split('*')
                    .try_fold(value.as_str(), |tail, part| {
                        tail.find(part).map(|index| &tail[index + part.len()..])
                    })
                    .is_none()
            });
            if unmatched.is_empty() {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduler_journal_search_matches_all_terms_and_honors_an_empty_column_selection() {
        let fields = [
            (Some(JournalColumn::Job), "BACKlog UPdate"),
            (Some(JournalColumn::Status), "Successful"),
            (None, "internal-id"),
        ];
        let mut search = Search::default();
        assert_eq!(search.column_count(), 4);
        search.set_text("back*up success internal-id".into());
        assert!(search.matches(fields));
        search.set_mode(JournalSearchMode::SelectedColumns);
        assert!(!search.matches(fields));
        search.set_text("back*up success".into());
        assert!(search.matches(fields));
        search.set_text("up*back".into());
        assert!(!search.matches(fields));
        search.set_text("*".into());
        search.toggle_columns();
        for column in JournalColumn::ALL {
            search.set_column(column, false);
        }
        assert!(!search.matches(fields));
        search.set_text(" ".into());
        assert!(search.matches(fields));
    }
}
