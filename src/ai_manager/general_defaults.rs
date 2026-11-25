use super::*;
use crate::workspace::ai_general_defaults::SnippetValues;

pub(super) fn shortcuts(content: Element<'_, Message>) -> Element<'_, Message> {
    dialog_design::on_key_press(content, |key, modifiers| {
        if !modifiers.command() || modifiers.alt() {
            return None;
        }
        let iced::keyboard::Key::Character(value) = key else {
            return None;
        };
        if value.eq_ignore_ascii_case("s") {
            Some(Message::Save)
        } else if value.eq_ignore_ascii_case("f") {
            Some(Message::Find)
        } else if value.eq_ignore_ascii_case("w") {
            Some(Message::Close)
        } else {
            None
        }
    })
}
use iced::widget::column;

impl Dialog {
    pub(crate) fn install_snippet_defaults(&mut self, values: SnippetValues) {
        self.shared_defaults.install_snippets(values);
    }

    pub(crate) fn take_shared_default_updates(
        &mut self,
    ) -> (
        Option<SnippetValues>,
        Option<crate::terminal_agent::history::Snapshot>,
    ) {
        self.shared_defaults.take_updates()
    }

    pub(super) fn agent_history_limit_view(&self) -> Element<'_, Message> {
        use search::Setting;
        let setting = Setting::AgentHistoryLimit;
        let enabled = self.editable() && self.shared_defaults.history_available();
        let input = text_input("5–100", &self.shared_defaults.history_limit)
            .on_input_maybe(enabled.then_some(Message::AgentHistoryLimit))
            .width(80);
        self.revealed.wrap(setting, column![
            row![text(setting.title()).size(13).width(212), self.revealed.input(setting, enabled, input)]
                .spacing(12).align_y(Alignment::Center),
            text("Keep 5–100 recent prompts. Lowering this limit removes older saved prompts when you Save.").size(12),
            (!self.shared_defaults.problem.is_empty()).then(|| text(&self.shared_defaults.problem).size(12)),
        ].spacing(6))
    }

    pub(super) fn snippet_defaults_view(&self) -> Element<'_, Message> {
        use crate::snippet_window::ai_assist::{ALTERNATIVE_COUNTS, DEFAULT_ALTERNATIVE_COUNT};
        use search::Setting;
        let values = self.shared_defaults.snippets();
        let enabled = self.editable() && values.is_some();
        let instructions = values.is_some_and(|value| value.instructions);
        let toggle = self.revealed.keyboard_action(
            Setting::SnippetInstructions,
            checkbox(instructions)
                .label(Setting::SnippetInstructions.title())
                .on_toggle_maybe(enabled.then_some(Message::SnippetInstructions)),
            enabled.then_some(Message::SnippetInstructions(!instructions)),
            false,
        );
        let selected = values.map_or(DEFAULT_ALTERNATIVE_COUNT, |value| value.alternatives);
        let count: Element<'_, Message> = if enabled {
            pick_list(
                ALTERNATIVE_COUNTS.collect::<Vec<_>>(),
                Some(selected),
                Message::SnippetAlternatives,
            )
            .keyboard_navigation(self.revealed.control_id(Setting::SnippetAlternatives))
            .dismiss_on_escape()
        } else {
            text(selected.to_string()).into()
        };
        column![
            self.revealed.wrap(Setting::SnippetInstructions, toggle),
            self.revealed.wrap(
                Setting::SnippetAlternatives,
                row![text(Setting::SnippetAlternatives.title()).size(13), count]
                    .spacing(12)
                    .align_y(Alignment::Center)
            ),
            (!self.shared_defaults.problem.is_empty())
                .then(|| text(&self.shared_defaults.problem).size(12)),
        ]
        .spacing(8)
        .into()
    }
}
