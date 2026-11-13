use iced::widget::{
    button, column, container, pick_list, responsive, row, scrollable, text, text_editor,
};
use iced::{Alignment, Element, Font, Length};

use crate::ai_manager::{MAX_RULE_CHARACTERS, rule_normalized_length};

use super::{Message, RuleDestination};

pub(super) fn editor<'a>(
    draft: &'a text_editor::Content,
    targets: &'a [RuleDestination],
    selected: Option<&'a RuleDestination>,
    proposal: Option<(&'a str, &'a str)>,
    busy: bool,
) -> Element<'a, Message> {
    let selected = selected.filter(|target| targets.contains(target));
    let (body, primary) = if let Some(((current, proposed), destination)) = proposal.zip(selected) {
        (
            review(current, proposed, destination),
            button("Save rule").on_press_maybe((!busy).then_some(Message::ConfirmRule)),
        )
    } else {
        let length = rule_normalized_length(&draft.text());
        let ready = !busy && (1..=MAX_RULE_CHARACTERS).contains(&length) && selected.is_some();
        (
            edit(draft, targets, selected, length, busy),
            button("Review rule").on_press_maybe(ready.then_some(Message::ReviewRule)),
        )
    };
    column![
        text("Remember as a rule").size(19),
        body,
        row![
            primary,
            proposal.is_some().then(|| button("Edit rule")
                .style(button::secondary)
                .on_press_maybe((!busy).then_some(Message::EditRule))),
            button("Cancel")
                .style(button::secondary)
                .on_press_maybe((!busy).then_some(Message::CancelRule)),
        ]
        .spacing(8)
        .wrap(),
    ]
    .spacing(8)
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn edit<'a>(
    draft: &'a text_editor::Content,
    targets: &'a [RuleDestination],
    selected: Option<&'a RuleDestination>,
    length: usize,
    busy: bool,
) -> Element<'a, Message> {
    let input = text_editor(draft)
        .height(110)
        .placeholder("Write a rule for future requests");
    let input = if busy {
        input
    } else {
        input.on_action(Message::RuleEdit)
    };
    let destination: Element<'a, Message> =
        if !busy && !targets.is_empty() && (selected.is_none() || targets.len() > 1) {
            pick_list(targets, selected, Message::RuleTarget)
                .placeholder("Choose a destination")
                .width(Length::Fill)
                .into()
        } else {
            let label = match selected {
                Some(destination) => destination.to_string(),
                None if targets.is_empty() => "No destination available for this rule.".into(),
                None => "No destination selected.".into(),
            };
            text(label).wrapping(text::Wrapping::WordOrGlyph).into()
        };
    scrollable(
        column![
            text("Rule text").size(14),
            input,
            text(format!("{length} / {MAX_RULE_CHARACTERS} characters")).size(13),
            row![text("Destination"), destination]
                .spacing(8)
                .align_y(Alignment::Center),
        ]
        .spacing(8)
        .width(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn review<'a>(
    current: &'a str,
    proposed: &'a str,
    selected: &RuleDestination,
) -> Element<'a, Message> {
    let skill = selected.is_skill();
    let (current_label, empty) = if skill {
        ("Current skill", "No current skill")
    } else {
        ("Current file", "Empty file")
    };
    let addition = if skill {
        None
    } else {
        proposed
            .strip_prefix(current)
            .and_then(|tail| tail.lines().find_map(|line| line.strip_prefix("- ")))
    };
    let files = responsive(move |size| {
        let current = file(current_label, current, empty);
        let proposed = file("After saving", proposed, empty);
        if size.width < 540.0 {
            column![current, proposed]
                .spacing(8)
                .height(Length::Fill)
                .into()
        } else {
            row![current, proposed]
                .spacing(10)
                .height(Length::Fill)
                .into()
        }
    });
    column![
        text(format!("Destination · {selected}")).wrapping(text::Wrapping::WordOrGlyph),
        addition.map(|value| container(scrollable(
            text(value)
                .size(14)
                .width(Length::Fill)
                .wrapping(text::Wrapping::WordOrGlyph),
        ))
        .max_height(56)),
        files,
    ]
    .spacing(8)
    .height(Length::Fill)
    .width(Length::Fill)
    .into()
}

fn file<'a>(label: &'static str, content: &'a str, empty: &'static str) -> Element<'a, Message> {
    let body = if content.is_empty() {
        text(empty).size(13)
    } else {
        text(content)
            .font(Font::MONOSPACE)
            .size(13)
            .wrapping(text::Wrapping::WordOrGlyph)
    };
    container(
        column![
            text(label).size(14),
            scrollable(body.width(Length::Fill))
                .width(Length::Fill)
                .height(Length::Fill),
        ]
        .spacing(6),
    )
    .padding(8)
    .style(container::bordered_box)
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}
