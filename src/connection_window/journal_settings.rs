use iced::widget::{checkbox, column, row, rule, text, text_input};
use iced::{Alignment, Element, Length};

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

#[derive(Clone, Copy)]
pub(crate) struct Policy {
    pub(crate) allowed: bool,
    pub(crate) required: bool,
    pub(crate) summaries_allowed: bool,
    pub(crate) max_parts: Option<u16>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            allowed: true,
            required: false,
            summaries_allowed: true,
            max_parts: None,
        }
    }
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

    pub(super) fn view(&self, policy: Policy) -> Element<'_, Edit> {
        let clamp = move |value: String| match (value.parse::<u16>(), policy.max_parts) {
            (Ok(parts), Some(cap)) if parts > cap => cap.to_string(),
            _ => value,
        };
        let parts = clamp(self.parts.clone());
        let label = policy.max_parts.map_or_else(
            || "Maximum capture-log parts".into(),
            |cap| format!("Maximum capture-log parts (managed limit {cap})"),
        );
        let summaries_allowed = policy.allowed && policy.summaries_allowed;
        let interval_editable = summaries_allowed && self.settings.ai_summaries;
        let field = |label: String, input| {
            row![text(label).width(Length::Fill), input]
                .spacing(10)
                .align_y(Alignment::Center)
        };
        column![
            checkbox(policy.required || self.settings.auto_start)
                .label("Start a journal on connect")
                .on_toggle_maybe((policy.allowed && !policy.required).then_some(Edit::Auto)),
            policy
                .required
                .then(|| text("Session journaling is required by managed policy.").size(13)),
            rule::horizontal(1),
            checkbox(self.settings.capture_input)
                .label("Capture typed commands at recognized shell prompts")
                .on_toggle_maybe(policy.allowed.then_some(Edit::Input)),
            checkbox(summaries_allowed && self.settings.ai_summaries)
                .label("Allow AI summaries")
                .on_toggle_maybe(summaries_allowed.then_some(Edit::Summaries)),
            field(
                "Summary interval in minutes (0 uses the global default)".into(),
                text_input("0", &self.interval)
                    .on_input_maybe(interval_editable.then_some(Edit::Interval))
                    .width(110)
            ),
            field(
                "Capture-log part size in MiB".into(),
                text_input("25", &self.part_size)
                    .on_input_maybe(policy.allowed.then_some(Edit::PartSize))
                    .width(110)
            ),
            field(
                label,
                text_input("20", &parts)
                    .on_input_maybe(
                        policy
                            .allowed
                            .then_some(move |value| Edit::Parts(clamp(value)))
                    )
                    .width(110)
            ),
            text("Typed command capture starts at the next recognized shell-integration prompt.")
                .size(13)
                .style(text::secondary),
            (!policy.allowed).then(|| text("Journal capture is disabled by policy.").size(13)),
            (policy.allowed && !policy.summaries_allowed)
                .then(|| text("Journal AI summaries are disabled by policy.").size(13)),
        ]
        .spacing(10)
        .into()
    }
}
