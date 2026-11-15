//! Editor for a job's separate, opt-in session journal capture and retention.

use std::fmt;

use iced::Element;
use iced::widget::{button, checkbox, column, pick_list, row, text, text_input};

use crate::ai_manager::JavaProfileSource;
use crate::session_journal::automation::{Closing, Keep, Options, Retention};
use crate::session_journal::summary_queue::ProfileChoice;

mod estimate;
pub(super) use estimate::Subject;
pub(crate) use estimate::{Estimate, ModelEstimate, Request};

#[derive(Clone, Debug)]
pub enum Event {
    Enabled(bool),
    Input(bool),
    Keep(Keep),
    Closing(Closing),
    SummaryProfile(Box<SummaryProvider>),
    UseImportedProfile(Box<SummaryProvider>),
    Retention(RetentionMode),
    Days(String),
    Date(String),
    Runs(String),
    Storage(String),
    Deduplicate(bool),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SummaryProvider(
    pub(crate) Option<ProfileChoice>,
    pub(crate) String,
    pub(crate) Option<JavaProfileSource>,
);

impl fmt::Display for SummaryProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.1)
    }
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
    pub(crate) fn summary_profile(&self) -> Option<&ProfileChoice> {
        self.options.summary_profile.as_ref()
    }

    pub(crate) fn unresolved_summary_profile(&self) -> bool {
        self.options.unresolved_summary_profile.is_some()
    }

    fn matches_imported_profile(&self, choice: &SummaryProvider) -> bool {
        self.unresolved_summary_profile()
            && choice.0.is_some()
            && choice
                .2
                .as_ref()
                .is_some_and(|source| self.options.java_profile_source.as_ref() == Some(source))
    }

    fn imported_profile<'a>(&self, profiles: &'a [SummaryProvider]) -> Option<&'a SummaryProvider> {
        let mut matches = profiles
            .iter()
            .filter(|choice| self.matches_imported_profile(choice));
        let choice = matches.next()?;
        matches.next().is_none().then_some(choice)
    }

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
            Event::SummaryProfile(choice) => self.select_summary_profile(choice.0),
            Event::UseImportedProfile(choice) if self.matches_imported_profile(&choice) => {
                self.select_summary_profile(choice.0);
            }
            Event::Retention(value) => self.retention = value,
            Event::Days(value) if value.len() <= 4 => self.days = value,
            Event::Date(value) if value.len() <= 10 => self.date = value,
            Event::Runs(value) if value.len() <= 4 => self.runs = value,
            Event::Storage(value) if value.len() <= 7 => self.storage = value,
            Event::Deduplicate(value) => self.options.deduplicate = value,
            _ => {}
        }
    }

    fn select_summary_profile(&mut self, profile: Option<ProfileChoice>) {
        self.options.summary_profile = profile;
        self.options.unresolved_summary_profile = None;
        self.options.java_profile_source = None;
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

    pub(super) fn view(
        &self,
        file_action: bool,
        virtual_terminal: bool,
        profiles: Vec<SummaryProvider>,
    ) -> Element<'_, Event> {
        self.view_with_input(
            Some(if file_action {
                "Include the file action and paths for each target"
            } else {
                "Include the commands sent to each target"
            }),
            virtual_terminal,
            profiles,
        )
    }

    pub(crate) fn swarm_view(&self, profiles: Vec<SummaryProvider>) -> Element<'_, Event> {
        self.view_with_input(None, false, profiles)
    }

    fn view_with_input(
        &self,
        capture_input_label: Option<&'static str>,
        virtual_terminal: bool,
        profiles: Vec<SummaryProvider>,
    ) -> Element<'_, Event> {
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
        if virtual_terminal {
            controls = controls
                .push(text("The journal includes redacted virtual terminal screenshots.").size(12));
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
            let unresolved = self.unresolved_summary_profile();
            let selected = if unresolved {
                None
            } else {
                profiles
                    .iter()
                    .find(|choice| choice.0.as_ref() == self.summary_profile())
                    .cloned()
                    .or_else(|| {
                        let profile = self.summary_profile()?;
                        Some(SummaryProvider(
                            Some(profile.clone()),
                            format!("Unavailable · {} · {}", profile.model, profile.destination),
                            None,
                        ))
                    })
            };
            controls = controls.push(text("Closing summary profile").size(12));
            if let Some(choice) = self.imported_profile(&profiles) {
                controls = controls
                    .push(text(format!("Imported profile · {choice}")).size(12))
                    .push(
                        button("Use imported profile")
                            .on_press(Event::UseImportedProfile(Box::new(choice.clone()))),
                    );
            } else if unresolved {
                controls = controls.push(
                    text("Review the imported profile in AI Manager when available, or select an available profile here.")
                        .size(12),
                );
            }
            controls = controls
                .push(
                    pick_list(profiles, selected, |choice| {
                        Event::SummaryProfile(Box::new(choice))
                    })
                    .placeholder("Select a profile for the imported journal"),
                )
                .push(
                    text("Review outbound access for this source in Journal Summary settings.")
                        .size(12),
                );
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_manager::ProfileRef;

    #[test]
    fn an_exact_imported_profile_is_offered_without_selecting_it() {
        let source = |fingerprint: u8| -> JavaProfileSource {
            let fingerprint = [fingerprint; 32];
            serde_json::from_value(serde_json::json!({
                "id": "legacy-summary-profile",
                "fingerprint": fingerprint,
            }))
            .unwrap()
        };
        let profile = |id| ProfileChoice {
            id: ProfileRef::Numeric(id),
            model: "fixture".into(),
            destination: "127.0.0.1:11434".into(),
            policy_model_fingerprint: None,
            https: None,
            cli: None,
        };
        let imported = Options {
            enabled: true,
            closing: Closing::Always,
            unresolved_summary_profile: Some("legacy-summary-profile".into()),
            java_profile_source: Some(source(1)),
            ..Options::default()
        };
        let matching = SummaryProvider(Some(profile(1)), "Team profile".into(), Some(source(1)));
        let wrong = SummaryProvider(Some(profile(2)), "Team profile".into(), Some(source(2)));
        let existing = SummaryProvider(Some(profile(3)), "Team profile".into(), None);
        let other_copy = SummaryProvider(Some(profile(4)), "Team profile".into(), Some(source(1)));
        let default = SummaryProvider(None, "Use the automation journal profile".into(), None);
        let mut draft = Draft::from_options(Some(&imported));

        assert!(
            draft
                .imported_profile(&[wrong.clone(), existing.clone()])
                .is_none()
        );
        assert!(
            draft
                .imported_profile(&[matching.clone(), other_copy])
                .is_none()
        );
        let choices = [wrong.clone(), existing, matching.clone()];
        assert_eq!(draft.imported_profile(&choices), Some(&matching));
        assert_eq!(draft.build_options().unwrap(), imported);

        draft.edit(Event::UseImportedProfile(Box::new(wrong)));
        assert_eq!(draft.build_options().unwrap(), imported);

        draft.edit(Event::UseImportedProfile(Box::new(matching)));
        let selected = draft.build_options().unwrap();
        assert_eq!(selected.summary_profile, Some(profile(1)));
        assert!(selected.unresolved_summary_profile.is_none());
        assert!(selected.java_profile_source.is_none());
        assert!(draft.imported_profile(&choices).is_none());

        let mut draft = Draft::from_options(Some(&imported));
        draft.edit(Event::SummaryProfile(Box::new(default)));
        let selected = draft.build_options().unwrap();
        assert!(selected.summary_profile.is_none());
        assert!(selected.unresolved_summary_profile.is_none());
        assert!(selected.java_profile_source.is_none());
    }
}
