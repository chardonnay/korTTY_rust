use iced::Element;
use iced::widget::{checkbox, column, text, text_input};

use crate::session_journal::{
    ConnectionConfig, MAX_CONNECTION_JOURNAL_PARTS, MAX_PART_MIB, MAX_SUMMARY_MINUTES,
};

#[derive(Clone, Debug)]
pub enum Edit {
    Auto(bool),
    Input(bool),
    Summaries(bool),
    Interval(String),
    PartSize(String),
    Parts(String),
}

pub(super) struct Draft {
    settings: ConnectionConfig,
    interval: String,
    part_size: String,
    parts: String,
}

impl Draft {
    pub(super) fn new(settings: ConnectionConfig) -> Self {
        Self {
            interval: settings.summary_interval_minutes.to_string(),
            part_size: settings.max_log_size_mib.to_string(),
            parts: settings.max_log_parts.to_string(),
            settings,
        }
    }

    pub(super) fn update(&mut self, edit: Edit) {
        match edit {
            Edit::Auto(enabled) => self.settings.auto_start = enabled,
            Edit::Input(enabled) => self.settings.capture_input = enabled,
            Edit::Summaries(enabled) => self.settings.ai_summaries = enabled,
            Edit::Interval(value) => self.interval = value.chars().take(4).collect(),
            Edit::PartSize(value) => self.part_size = value.chars().take(3).collect(),
            Edit::Parts(value) => self.parts = value.chars().take(2).collect(),
        }
    }

    pub(super) fn value(&self) -> Result<ConnectionConfig, String> {
        let number = |raw: &str, min: u16, max: u16, label: &str| {
            raw.trim()
                .parse::<u16>()
                .ok()
                .filter(|value| (min..=max).contains(value))
                .ok_or_else(|| format!("Enter {label} from {min} to {max}."))
        };
        Ok(ConnectionConfig {
            summary_interval_minutes: number(
                &self.interval,
                0,
                MAX_SUMMARY_MINUTES,
                "summary interval minutes",
            )?,
            max_log_size_mib: number(&self.part_size, 1, MAX_PART_MIB, "journal part size in MiB")?,
            max_log_parts: number(
                &self.parts,
                1,
                MAX_CONNECTION_JOURNAL_PARTS,
                "maximum journal parts",
            )?,
            ..self.settings
        })
    }

    pub(super) fn view(&self, managed_max: Option<u16>) -> Element<'_, Edit> {
        let clamp = move |value: String| match (value.parse::<u16>(), managed_max) {
            (Ok(parts), Some(cap)) if parts > cap => cap.to_string(),
            _ => value,
        };
        let parts = clamp(self.parts.clone());
        let label = managed_max.map_or_else(
            || "Maximum capture-log parts".into(),
            |cap| format!("Maximum capture-log parts (managed limit {cap})"),
        );
        column![
            text("Session journal").size(16),
            checkbox(self.settings.auto_start)
                .label("Start a journal on connect")
                .on_toggle(Edit::Auto),
            checkbox(self.settings.capture_input)
                .label("Capture typed commands at recognized shell prompts")
                .on_toggle(Edit::Input),
            text("Typed command capture starts at the next recognized shell-integration prompt."),
            checkbox(self.settings.ai_summaries)
                .label("Allow AI summaries")
                .on_toggle(Edit::Summaries),
            text("Summary interval in minutes (0 uses the global default)"),
            text_input("0", &self.interval).on_input(Edit::Interval),
            text("Capture-log part size in MiB"),
            text_input("25", &self.part_size).on_input(Edit::PartSize),
            text(label),
            text_input("20", &parts).on_input(move |value| Edit::Parts(clamp(value))),
        ]
        .spacing(5)
        .into()
    }
}
