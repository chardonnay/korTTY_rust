//! Editor for a job's separate, opt-in session journal capture and retention.

use std::fmt;

use iced::Element;
use iced::widget::{checkbox, column, pick_list, row, text, text_input};

use crate::session_journal::automation::{Closing, Keep, Options, Retention};

mod estimate;
pub(super) use estimate::Subject;
pub(crate) use estimate::{Estimate, Request};

#[derive(Clone, Debug)]
pub enum Event {
    Enabled(bool),
    Input(bool),
    Keep(Keep),
    Closing(Closing),
    Retention(RetentionMode),
    Days(String),
    Date(String),
    Runs(String),
    Storage(String),
    Deduplicate(bool),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetentionMode {
    Days,
    Date,
    Forever,
}

impl fmt::Display for RetentionMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Days => "Days after completion",
            Self::Date => "Fixed date",
            Self::Forever => "No expiry",
        })
    }
}

impl fmt::Display for Keep {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Always => "Keep all outcomes",
            Self::Failures => "Keep failed, blocked and cancelled runs",
        })
    }
}

impl fmt::Display for Closing {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Off => "Keep the original capture",
            Self::Always => "Summarize kept journals",
            Self::Failures => "Summarize kept failures",
        })
    }
}

pub(crate) struct Draft {
    options: Options,
    retention: RetentionMode,
    days: String,
    date: String,
    runs: String,
    storage: String,
    pub(super) estimate: estimate::Panel,
}

impl Draft {
    pub(crate) fn estimate_mode(&self) -> Option<(Closing, Keep)> {
        (self.options.enabled && self.options.closing != Closing::Off)
            .then_some((self.options.closing, self.options.keep))
    }

    pub(crate) fn from_options(options: Option<&Options>) -> Self {
        let options = options.cloned().unwrap_or_default();
        let mut days = "14".to_owned();
        let mut date = String::new();
        let retention = match &options.retention {
            Retention::Days { days: count } => {
                days = count.to_string();
                RetentionMode::Days
            }
            Retention::Date { date: value } => {
                date.clone_from(value);
                RetentionMode::Date
            }
            Retention::Forever => RetentionMode::Forever,
        };
        let runs = options.max_runs.to_string();
        let storage = options.max_storage_mib.to_string();
        Self {
            options,
            retention,
            days,
            date,
            runs,
            storage,
            estimate: estimate::Panel::default(),
        }
    }

    pub(crate) fn edit(&mut self, event: Event) {
        match event {
            Event::Enabled(value) => self.options.enabled = value,
            Event::Input(value) => self.options.capture_input = value,
            Event::Keep(value) => self.options.keep = value,
            Event::Closing(value) => self.options.closing = value,
            Event::Retention(value) => self.retention = value,
            Event::Days(value) if value.len() <= 4 => self.days = value,
            Event::Date(value) if value.len() <= 10 => self.date = value,
            Event::Runs(value) if value.len() <= 4 => self.runs = value,
            Event::Storage(value) if value.len() <= 7 => self.storage = value,
            Event::Deduplicate(value) => self.options.deduplicate = value,
            _ => {}
        }
    }

    pub(crate) fn build(&self) -> Result<Option<Options>, String> {
        if !self.options.enabled {
            return Ok(None);
        }
        self.build_options().map(Some)
    }

    pub(crate) fn build_options(&self) -> Result<Options, String> {
        let mut options = self.options.clone();
        options.retention = match self.retention {
            RetentionMode::Days => Retention::Days {
                days: self
                    .days
                    .trim()
                    .parse()
                    .map_err(|_| "Enter the number of days to keep automation journals")?,
            },
            RetentionMode::Date => Retention::Date {
                date: self.date.trim().into(),
            },
            RetentionMode::Forever => Retention::Forever,
        };
        options.max_runs = self
            .runs
            .trim()
            .parse()
            .map_err(|_| "Enter a maximum number of runs or zero for no limit")?;
        options.max_storage_mib = self
            .storage
            .trim()
            .parse()
            .map_err(|_| "Enter a storage limit in MiB or zero for no limit")?;
        options.validate().map_err(|error| error.to_string())?;
        Ok(options)
    }

    pub(super) fn view(&self, file_action: bool) -> Element<'_, Event> {
        self.view_with_input(Some(if file_action {
            "Include the file action and paths for each target"
        } else {
            "Include the commands sent to each target"
        }))
    }

    pub(crate) fn swarm_view(&self) -> Element<'_, Event> {
        self.view_with_input(None)
    }

    fn view_with_input(&self, capture_input_label: Option<&'static str>) -> Element<'_, Event> {
        let mut controls = column![
            checkbox(self.options.enabled)
                .label("Record a session journal per target")
                .on_toggle(Event::Enabled)
        ]
        .spacing(6);
        if !self.options.enabled {
            return controls.into();
        }
        if let Some(label) = capture_input_label {
            controls = controls.push(
                checkbox(self.options.capture_input)
                    .label(label)
                    .on_toggle(Event::Input),
            );
        }
        controls = controls.push(
            row![
                pick_list(
                    [Keep::Always, Keep::Failures],
                    Some(self.options.keep),
                    Event::Keep
                ),
                pick_list(
                    [Closing::Off, Closing::Always, Closing::Failures],
                    Some(self.options.closing),
                    Event::Closing
                ),
            ]
            .spacing(8)
            .wrap(),
        );
        if self.options.closing != Closing::Off {
            controls = controls.push(text("Closing summaries send masked activity to the configured local journal model and may use its token quota.").size(12));
        }
        controls = controls.push(pick_list(
            [
                RetentionMode::Days,
                RetentionMode::Date,
                RetentionMode::Forever,
            ],
            Some(self.retention),
            Event::Retention,
        ));
        controls = match self.retention {
            RetentionMode::Days => {
                controls.push(text_input("Days after completion", &self.days).on_input(Event::Days))
            }
            RetentionMode::Date => controls
                .push(text_input("Expiry date (YYYY-MM-DD)", &self.date).on_input(Event::Date)),
            RetentionMode::Forever => controls,
        };
        controls
            .push(
                row![
                    column![
                        text("Maximum runs · zero for no limit").size(12),
                        text_input("Maximum runs", &self.runs).on_input(Event::Runs)
                    ],
                    column![
                        text("Storage MiB · zero for no limit").size(12),
                        text_input("Storage MiB", &self.storage).on_input(Event::Storage)
                    ],
                ]
                .spacing(8)
                .wrap(),
            )
            .push(
                checkbox(self.options.deduplicate)
                    .label("Keep a single copy of identical target activity")
                    .on_toggle(Event::Deduplicate),
            )
            .into()
    }
}
