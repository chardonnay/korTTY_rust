use iced::widget::{Column, button, container};
use iced::{Element, Length, Point};

use super::chrome::popup;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Choice {
    #[default]
    CopyImage,
    CopyPath,
    Edit,
    Analyze,
    Export,
}

impl Choice {
    fn choices(analysis: bool) -> &'static [Self] {
        if analysis {
            &[
                Self::CopyImage,
                Self::CopyPath,
                Self::Edit,
                Self::Analyze,
                Self::Export,
            ]
        } else {
            &[Self::CopyImage, Self::CopyPath, Self::Edit, Self::Export]
        }
    }

    pub(super) fn step(self, forward: bool, analysis: bool) -> Self {
        let choices = Self::choices(analysis);
        let position = choices
            .iter()
            .position(|choice| *choice == self)
            .unwrap_or(0);
        let offset = if forward { 1 } else { choices.len() - 1 };
        choices[(position + offset) % choices.len()]
    }

    fn label(self) -> &'static str {
        match self {
            Self::CopyImage => "Copy screenshot",
            Self::CopyPath => "Copy screenshot path",
            Self::Edit => "Edit screenshot…",
            Self::Analyze => "Analyze screenshot with AI",
            Self::Export => "Export screenshot…",
        }
    }
}

pub(super) fn menu<'a, Message: Clone + 'a>(
    trigger: Element<'a, Message>,
    active: Option<(Point, Choice)>,
    analysis: bool,
    open: impl Fn(Point) -> Message + 'a,
    choose: impl Fn(Choice) -> Message,
    step: impl Fn(bool) -> Message,
    dismiss: Message,
) -> Element<'a, Message> {
    let choices = Choice::choices(analysis);
    let selection = active.map_or_else(Choice::default, |(_, choice)| {
        if choices.contains(&choice) {
            choice
        } else {
            Choice::default()
        }
    });
    let contents = active.map(|(point, _)| {
        let buttons = choices.iter().copied().map(|choice| {
            button(choice.label())
                .width(Length::Fill)
                .style(if choice == selection {
                    button::secondary
                } else {
                    button::text
                })
                .on_press(choose(choice))
                .into()
        });
        let controls = container(Column::with_children(buttons).spacing(2))
            .width(220)
            .padding(6)
            .style(container::bordered_box);
        (point, controls.into())
    });
    popup::context_action(
        trigger,
        open,
        contents,
        choose(selection),
        dismiss,
        Some([step(false), step(true)]),
    )
}
