use iced::widget::{checkbox, column, row, rule, text, text_input};
use iced::{Alignment, Element, Length};

use crate::session_journal::{
    ConnectionConfig, DEFAULT_PART_LIMIT, MAX_CONNECTION_JOURNAL_PARTS, MAX_PART_MIB,
    MAX_SUMMARY_MINUTES,
};
use crate::workspace::dialog_design::{keyboard_table, number_input};

const LABEL_WIDTH: f32 = 260.0;
const INPUT_WIDTH: f32 = 96.0;

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
    pub(crate) max_parts: Option<u32>,
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
            Edit::Auto(enabled) => {
                self.settings.auto_start = enabled;
                if !enabled {
                    for (input, saved, min, max) in [
                        (
                            &mut self.interval,
                            u32::from(self.settings.summary_interval_minutes),
                            0,
                            u32::from(MAX_SUMMARY_MINUTES),
                        ),
                        (
                            &mut self.part_size,
                            u32::from(self.settings.max_log_size_mib),
                            1,
                            u32::from(MAX_PART_MIB),
                        ),
                        (
                            &mut self.parts,
                            self.settings.max_log_parts,
                            1,
                            MAX_CONNECTION_JOURNAL_PARTS,
                        ),
                    ] {
                        if bounded(input, min, max).is_none() {
                            *input = saved.to_string();
                        }
                    }
                }
            }
            Edit::Input(enabled) => self.settings.capture_input = enabled,
            Edit::Summaries(enabled) => self.settings.ai_summaries = enabled,
            Edit::Interval(value) => self.interval = value.chars().take(4).collect(),
            Edit::PartSize(value) => self.part_size = value.chars().take(4).collect(),
            Edit::Parts(value) => self.parts = value.chars().take(11).collect(),
        }
    }

    pub(super) fn value(&self) -> Result<ConnectionConfig, String> {
        let number = |raw: &str, min: u32, max: u32, label: &str| {
            bounded(raw, min, max).ok_or_else(|| format!("Enter {label} from {min} to {max}."))
        };
        Ok(ConnectionConfig {
            summary_interval_minutes: number(
                &self.interval,
                0,
                u32::from(MAX_SUMMARY_MINUTES),
                "summary interval minutes",
            )? as u16,
            max_log_size_mib: number(
                &self.part_size,
                1,
                u32::from(MAX_PART_MIB),
                "journal part size in MiB",
            )? as u16,
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
        let part_limit = policy.max_parts.unwrap_or(DEFAULT_PART_LIMIT);
        let clamp = move |value: String| match value.trim().parse::<u32>() {
            Ok(parts) if parts > part_limit => part_limit.to_string(),
            _ => value,
        };
        let parts = clamp(self.parts.clone());
        let enabled = policy.required || self.settings.auto_start;
        let options_editable = enabled && policy.allowed;
        let summary_editable = options_editable && policy.summaries_allowed;
        let interval_editable = summary_editable && self.settings.ai_summaries;
        let interval_edit = interval_editable.then_some(Edit::Interval as fn(String) -> Edit);
        let size_edit = options_editable.then_some(Edit::PartSize as fn(String) -> Edit);
        let parts_edit = options_editable.then_some(Edit::Parts as fn(String) -> Edit);
        let field = |label, editable, input| {
            let label = text(label).size(13).width(LABEL_WIDTH);
            let label = if editable {
                label
            } else {
                label.style(text::secondary)
            };
            row![label, input].spacing(10).align_y(Alignment::Center)
        };
        column![
            keyboard_table::action(checkbox(enabled)
                .label("Enable session journal for this connection")
                .text_size(13)
                .on_toggle_maybe((policy.allowed && !policy.required).then_some(Edit::Auto))
                .into(),
                (policy.allowed && !policy.required).then_some(Edit::Auto(!enabled)), false),
            rule::horizontal(1),
            column![
                keyboard_table::action(checkbox(self.settings.capture_input)
                    .label("Capture typed commands at recognized shell prompts")
                    .text_size(13)
                    .on_toggle_maybe(options_editable.then_some(Edit::Input)).into(),
                    options_editable.then_some(Edit::Input(!self.settings.capture_input)), false),
                keyboard_table::action(checkbox(policy.summaries_allowed && self.settings.ai_summaries)
                    .label("Generate AI summaries")
                    .text_size(13)
                    .on_toggle_maybe(summary_editable.then_some(Edit::Summaries)).into(),
                    summary_editable.then_some(Edit::Summaries(!self.settings.ai_summaries)), false),
                field(
                    "Summary interval (min, 0 = global default)",
                    interval_editable,
                    number_input(
                        text_input("0", &self.interval).on_input_maybe(interval_edit),
                        &self.interval,
                        0..=i64::from(MAX_SUMMARY_MINUTES),
                        1,
                        interval_edit,
                        INPUT_WIDTH,
                    ),
                ),
                field(
                    "Maximum size per log part (MiB)",
                    options_editable,
                    number_input(
                        text_input("25", &self.part_size).on_input_maybe(size_edit),
                        &self.part_size,
                        1..=i64::from(MAX_PART_MIB),
                        1,
                        size_edit,
                        INPUT_WIDTH,
                    ),
                ),
                field(
                    "Maximum rotated log parts",
                    options_editable,
                    number_input(
                        text_input("20", &parts).on_input_maybe(
                            options_editable.then_some(move |value| Edit::Parts(clamp(value))),
                        ),
                        &parts,
                        1..=i64::from(part_limit),
                        1,
                        parts_edit,
                        INPUT_WIDTH,
                    ),
                ),
                text("At the part limit, output capture stops. Typed input, screenshots and notes can still be recorded.")
                    .size(13)
                    .style(text::secondary),
            ]
            .spacing(10),
            text("Typed command capture starts at the next recognized shell-integration prompt.")
                .size(13)
                .style(text::secondary),
            (policy.allowed && policy.required)
                .then(|| text("Session journaling is required by managed policy.").size(13)),
            policy.max_parts.filter(|_| policy.allowed)
                .map(|cap| text(format!("Managed limit of {cap} rotated log parts.")).size(13)),
            (!policy.allowed).then(|| text("Journal capture is disabled by policy.").size(13)),
            (policy.allowed && !policy.summaries_allowed)
                .then(|| text("Journal AI summaries are disabled by policy.").size(13)),
        ]
        .spacing(15)
        .width(Length::Fill)
        .into()
    }
}

fn bounded(raw: &str, min: u32, max: u32) -> Option<u32> {
    raw.trim()
        .parse::<u32>()
        .ok()
        .filter(|value| (min..=max).contains(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings_search::test_input;
    use iced::advanced::widget::operation::focusable;
    use iced::advanced::widget::{Operation, operation};
    use iced::keyboard::{Key, key::Named};
    use iced::{Font, Renderer, Size};
    use iced_runtime::{Action, UserInterface, task, user_interface::Cache};

    fn next(interface: &mut UserInterface<'_, Edit, iced::Theme, Renderer>, renderer: &Renderer) {
        test_input::apply(
            interface,
            renderer,
            task::effect(Action::widget(focusable::focus_next())),
        );
    }

    #[test]
    fn keyboard_reaches_enabled_journal_options_and_skips_disabled_options() {
        let mut draft = Draft::new(ConnectionConfig::default());
        let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        {
            let mut interface = UserInterface::build(
                draft.view(Policy::default()),
                Size::new(820.0, 600.0),
                Cache::default(),
                &mut renderer,
            );
            next(&mut interface, &renderer);
            let messages = test_input::press(
                &mut interface,
                &mut renderer,
                Key::Named(Named::Space),
                None,
            );
            assert!(matches!(messages.as_slice(), [Edit::Auto(true)]));
            // Only the enable switch participates while the journal is off.
            let mut count = operation::map(focusable::count(), |count| {
                assert_eq!(count.total, 1);
            });
            interface.operate(&renderer, &mut count);
            assert!(matches!(count.finish(), operation::Outcome::Some(())));
        }
        draft.update(Edit::Auto(true));
        let mut interface = UserInterface::build(
            draft.view(Policy::default()),
            Size::new(820.0, 600.0),
            Cache::default(),
            &mut renderer,
        );
        next(&mut interface, &renderer);
        next(&mut interface, &renderer);
        assert!(matches!(
            test_input::press(
                &mut interface,
                &mut renderer,
                Key::Named(Named::Space),
                None
            )
            .as_slice(),
            [Edit::Input(true)]
        ));
        next(&mut interface, &renderer);
        assert!(matches!(
            test_input::press(
                &mut interface,
                &mut renderer,
                Key::Named(Named::Space),
                None
            )
            .as_slice(),
            [Edit::Summaries(false)]
        ));
        next(&mut interface, &renderer);
        assert!(
            matches!(test_input::press(&mut interface, &mut renderer, Key::Named(Named::ArrowUp), None).as_slice(), [Edit::Interval(value)] if value == "1")
        );
    }

    #[test]
    fn keyboard_skips_managed_switches_and_respects_the_rotation_limit() {
        let draft = Draft::new(ConnectionConfig::default());
        let policy = Policy {
            required: true,
            summaries_allowed: false,
            max_parts: Some(20),
            ..Policy::default()
        };
        let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        let mut interface = UserInterface::build(
            draft.view(policy),
            Size::new(820.0, 600.0),
            Cache::default(),
            &mut renderer,
        );
        next(&mut interface, &renderer);
        assert!(matches!(
            test_input::press(
                &mut interface,
                &mut renderer,
                Key::Named(Named::Space),
                None
            )
            .as_slice(),
            [Edit::Input(true)]
        ));
        next(&mut interface, &renderer);
        assert!(
            matches!(test_input::press(&mut interface, &mut renderer, Key::Named(Named::ArrowUp), None).as_slice(), [Edit::PartSize(value)] if value == "26")
        );
        next(&mut interface, &renderer);
        assert!(
            test_input::press(
                &mut interface,
                &mut renderer,
                Key::Named(Named::ArrowUp),
                None
            )
            .is_empty()
        );
        assert!(
            matches!(test_input::press(&mut interface, &mut renderer, Key::Named(Named::ArrowDown), None).as_slice(), [Edit::Parts(value)] if value == "19")
        );
    }

    #[test]
    fn journal_rotation_editor_keeps_java_sized_values() {
        let mut draft = Draft::new(ConnectionConfig::default());
        draft.update(Edit::PartSize("1000".into()));
        draft.update(Edit::Parts("2147483647".into()));
        let config = draft.value().unwrap();
        assert_eq!(config.max_log_size_mib, 1000);
        assert_eq!(config.max_log_parts, MAX_CONNECTION_JOURNAL_PARTS);
        draft.update(Edit::Parts("2147483648".into()));
        assert!(draft.value().is_err());
        draft.update(Edit::Parts("500".into()));
        draft.update(Edit::PartSize("1001".into()));
        assert!(draft.value().is_err());
    }

    #[test]
    fn disabling_journal_restores_invalid_fields_and_keeps_valid_edits() {
        let mut draft = Draft::new(ConnectionConfig::default());
        draft.update(Edit::Auto(true));
        draft.update(Edit::Interval(String::new()));
        draft.update(Edit::PartSize("50".into()));
        draft.update(Edit::Parts("2147483648".into()));
        assert!(draft.value().is_err());
        draft.update(Edit::Auto(false));
        let config = draft.value().unwrap();
        assert!(!config.auto_start);
        assert_eq!(config.summary_interval_minutes, 0);
        assert_eq!(config.max_log_size_mib, 50);
        assert_eq!(config.max_log_parts, 20);
    }
}
