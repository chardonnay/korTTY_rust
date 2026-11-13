use std::fmt;

use iced::widget::{
    button, checkbox, column, pick_list, row, scrollable, text, text_editor, text_input,
};
use iced::{Element, Font, Length};

use super::{
    Dialog, HardeningOption, InputOption, Language, MAX_WORKFLOW_FONT, MIN_WORKFLOW_FONT, Message,
    Results, Review, Stage, diagram,
};

const SUGGESTIONS: [u8; 5] = [1, 2, 3, 4, 5];

impl Dialog {
    pub(in crate::workspace::terminal_agent) fn view(&self) -> Element<'_, Message> {
        if let Some(review) = self.diagram.view() {
            let panel = column![
                review.map(Message::Flowchart),
                (!self.notice.is_empty()).then(|| text(&self.notice).size(13)),
            ]
            .spacing(8)
            .padding(12)
            .height(Length::Fill);
            return crate::workspace::dialog_placement::escape::on_escape(
                panel.into(),
                Message::Flowchart(diagram::Message::Cancel),
            );
        }
        let (body, actions): (Element<'_, Message>, Element<'_, Message>) = match &self.stage {
            Stage::Setup => (
                self.setup(),
                row![
                    button("Review requests").on_press(Message::Review),
                    button("Close").on_press(Message::Close),
                ]
                .spacing(6)
                .wrap()
                .into(),
            ),
            Stage::Review(review) => (
                review_requests(review, self.review_selected),
                row![
                    button("Generate drafts").on_press(Message::Generate),
                    button("Back").on_press(Message::Back),
                    button("Close").on_press(Message::Close),
                ]
                .spacing(6)
                .wrap()
                .into(),
            ),
            Stage::Running(pending) => (
                column![text(format!(
                    "Generating workflow drafts. {} of {} requests complete.",
                    pending.completed, pending.total
                ))]
                .height(Length::Fill)
                .into(),
                button("Cancel").on_press(Message::Cancel).into(),
            ),
            Stage::Results(results) => (
                draft_results(results, self.options.script_font),
                self.result_actions(results),
            ),
        };

        let content = column![
            text("Generate a workflow script").size(22),
            text(format!("AI profile · {}", self.profile_name)).size(13),
            text(format!("Source shell · {}", self.source_shell)).size(13),
            self.private.then(|| text(
                "Private run. Drafts stay in this dialog unless you copy them."
            )
            .size(13)),
            body,
            (!self.notice.is_empty()).then(|| text(&self.notice).size(13)),
            actions,
        ]
        .spacing(8)
        .padding(12)
        .height(Length::Fill);
        crate::workspace::dialog_placement::escape::on_escape(content.into(), Message::Close)
    }

    fn setup(&self) -> Element<'_, Message> {
        let options = &self.options;
        let mut additional = row![].spacing(8);
        for language in Language::GENERATION {
            if language != options.language {
                additional = additional.push(
                    checkbox(options.additional.contains(&language))
                        .label(language.to_string())
                        .on_toggle(move |checked| Message::Additional(language, checked)),
                );
            }
        }

        scrollable(
            column![
                text("Agent goal").size(16),
                text(&self.goal).wrapping(text::Wrapping::WordOrGlyph),
                row![
                    column![
                        text("Primary language").size(13),
                        pick_list(
                            Language::GENERATION,
                            Some(options.language),
                            Message::Language
                        ),
                    ]
                    .spacing(4),
                    column![
                        text("Suggestions per language").size(13),
                        pick_list(SUGGESTIONS, Some(options.suggestions), Message::Suggestions),
                    ]
                    .spacing(4),
                ]
                .spacing(10)
                .wrap(),
                text("Additional languages").size(13),
                additional.wrap(),
                checkbox(options.single_language)
                    .label("Keep each draft in its selected language")
                    .on_toggle(Message::SingleLanguage),
                text("Script header").size(13),
                row![
                    pick_list(
                        self.choices.as_slice(),
                        Some(options.header.clone()),
                        Message::Header
                    ),
                    button("Set default for primary language").on_press_maybe(
                        (!self.private && options.header.reference().is_some())
                            .then_some(Message::SetDefault),
                    ),
                ]
                .spacing(8)
                .wrap(),
                self.hardening_options(),
                text("Additional instructions").size(13),
                text_input("Optional requirements for the draft", &options.instructions)
                    .on_input(Message::Instructions),
            ]
            .spacing(8)
            .padding([4, 10])
            .width(Length::Fill),
        )
        .height(Length::Fill)
        .into()
    }

    fn hardening_options(&self) -> Element<'_, Message> {
        let options = &self.options;
        let hardening = &options.hardening;
        let mut controls = column![
            button(text(format!(
                "Hardening options ({})",
                hardening.classic.len()
            )))
            .on_press(Message::ExpandHardening)
        ]
        .spacing(6);
        if options.hardening_open {
            let mut classic = row![].spacing(8);
            for option in HardeningOption::ALL {
                classic = classic.push(
                    checkbox(hardening.classic.contains(&option))
                        .label(option.label())
                        .on_toggle(move |checked| Message::Hardening(option, checked)),
                );
            }
            controls = controls.push(classic.wrap()).push(
                row![
                    button("All").on_press(Message::AllHardening(true)),
                    button("Clear").on_press(Message::AllHardening(false)),
                    button("Save hardening defaults")
                        .on_press_maybe((!self.private).then_some(Message::SaveHardening)),
                ]
                .spacing(6)
                .wrap(),
            );
        }
        let supported = options.input_supported();
        let enabled = hardening.input_enabled && supported;
        let active = if enabled { hardening.input.len() } else { 0 };
        controls = controls.push(
            button(text(format!("Input hardening ({active})")))
                .on_press_maybe(supported.then_some(Message::ExpandInput)),
        );
        if !supported {
            return controls
                .push(text("Input hardening requires a script as the primary language.").size(13))
                .into();
        }
        if options.input_open {
            let mut guards = row![].spacing(8);
            for option in InputOption::ALL {
                guards = guards.push(
                    checkbox(hardening.input.contains(&option))
                        .label(option.label())
                        .on_toggle_maybe(
                            enabled.then_some(move |checked| Message::Guard(option, checked)),
                        ),
                );
            }
            controls = controls
                .push(
                    checkbox(enabled)
                        .label("Add a runtime input guard")
                        .on_toggle(Message::GuardEnabled),
                )
                .push(guards.wrap())
                .push(
                    row![
                        text("Maximum input file size (MiB)").size(13),
                        text_input("10", &options.max_file_mib)
                            .width(72)
                            .on_input_maybe(
                                options.file_size_enabled().then_some(Message::MaxFileMib)
                            ),
                        text("0 means unlimited").size(13),
                    ]
                    .spacing(8)
                    .wrap(),
                )
                .push(
                    (options.file_size_enabled() && options.parsed_file_size().is_err())
                        .then(|| text("Enter a size from 0 to 1024 MiB.").size(13)),
                )
                .push(
                    row![
                        button("All").on_press_maybe(enabled.then_some(Message::AllGuards(true))),
                        button("Clear")
                            .on_press_maybe(enabled.then_some(Message::AllGuards(false))),
                        button("Save input hardening defaults")
                            .on_press_maybe((!self.private).then_some(Message::SaveGuards)),
                    ]
                    .spacing(6)
                    .wrap(),
                );
        }
        controls.into()
    }

    fn result_actions(&self, results: &Results) -> Element<'_, Message> {
        let current = results.items.get(results.selected);
        let has_code = current.is_some_and(|item| item.content.is_some());
        let has_diagram = current.is_some_and(|item| item.diagram.is_some());
        let font = self.options.script_font;
        row![
            button("Copy draft").on_press_maybe(has_code.then_some(Message::Copy)),
            button("Open unsaved snippet")
                .on_press_maybe((has_code && !self.private).then_some(Message::OpenSnippet)),
            button("Workflow flowchart…")
                .on_press_maybe((has_code && !self.private).then_some(Message::Diagram)),
            (!self.private && has_diagram).then(|| button("Regenerate flowchart…")
                .on_press(Message::Flowchart(diagram::Message::Regenerate))),
            button("A−").on_press_maybe(
                (has_code && font > MIN_WORKFLOW_FONT).then(|| Message::ScriptFont(font - 1))
            ),
            button("A+").on_press_maybe(
                (has_code && font < MAX_WORKFLOW_FONT).then(|| Message::ScriptFont(font + 1))
            ),
            button("Back").on_press(Message::Back),
            button("Close").on_press(Message::Close),
        ]
        .spacing(6)
        .wrap()
        .into()
    }
}

fn review_requests(review: &Review, index: usize) -> Element<'_, Message> {
    let choices: Vec<_> = review
        .requests
        .iter()
        .enumerate()
        .map(|(index, request)| RequestChoice {
            index,
            language: request.language,
            suggestion: request.suggestion,
        })
        .collect();
    let selected = choices[index];
    let request = &review.requests[index];
    let heading = format!("Review {} model request(s)", choices.len());
    let picker = pick_list(choices, Some(selected), |choice| {
        Message::ReviewSelect(choice.index)
    });
    let request = column![
        text("System instructions").size(13),
        request_text(&request.system),
        text("User message").size(13),
        request_text(&request.input),
        text("Header added locally to the draft").size(13),
        request
            .header
            .text()
            .map_or_else(|| text("None"), request_text),
    ]
    .spacing(6)
    .width(Length::Fill);
    column![
        text(heading).size(17),
        text(format!("AI destination · {}", review.destination)).size(13),
        picker,
        scrollable(request.padding([4, 10])).height(Length::Fill),
    ]
    .spacing(8)
    .height(Length::Fill)
    .into()
}

#[derive(Clone, Copy, PartialEq)]
struct RequestChoice {
    index: usize,
    language: Language,
    suggestion: u8,
}

impl fmt::Display for RequestChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} · suggestion {}", self.language, self.suggestion)
    }
}

fn request_text(value: &str) -> text::Text<'_> {
    text(value)
        .font(Font::MONOSPACE)
        .size(13)
        .wrapping(text::Wrapping::WordOrGlyph)
}

#[derive(Clone, PartialEq)]
struct DraftChoice<'a> {
    index: usize,
    label: &'a str,
}

impl fmt::Display for DraftChoice<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label)
    }
}

fn draft_results(results: &Results, font: u8) -> Element<'_, Message> {
    let choices: Vec<_> = results
        .items
        .iter()
        .enumerate()
        .map(|(index, item)| DraftChoice {
            index,
            label: &item.label,
        })
        .collect();
    let selected = choices.get(results.selected).cloned();
    let mut body = column![
        text("Workflow drafts").size(17),
        pick_list(choices, selected, |choice| Message::Select(choice.index)),
    ]
    .spacing(8)
    .height(Length::Fill);
    if let Some(item) = results.items.get(results.selected) {
        body = body.push(text(format!("{} · {}", item.name, item.language)).size(13));
        if let Some(content) = &item.content {
            body = body.push(
                text_editor(content)
                    .font(Font::MONOSPACE)
                    .size(f32::from(font))
                    .height(Length::Fill)
                    .on_action(Message::Edit),
            );
        }
        if let Some(error) = &item.error {
            body = body.push(
                scrollable(text(error).wrapping(text::Wrapping::WordOrGlyph)).height(Length::Fill),
            );
        }
    }
    body.into()
}
