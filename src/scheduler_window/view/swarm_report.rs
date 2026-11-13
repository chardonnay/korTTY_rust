//! Read-only rendering of a finished Swarm run's persisted report.

use iced::widget::{column, rule, text};
use iced::{Element, Length};
use kortty_jobs::{RunOutcome, SwarmReport};

use super::{Message, outcome};

pub(super) fn summary(report: &SwarmReport) -> String {
    format!("Saved Swarm report · {} targets", report.targets.len())
}

pub(super) fn view(report: &SwarmReport) -> Element<'_, Message> {
    let mut details = column![
        rule::horizontal(1),
        text("Saved Swarm report").size(16),
        text("Question").size(12),
        text(&report.question).wrapping(text::Wrapping::WordOrGlyph),
    ]
    .spacing(6)
    .width(Length::Fill);

    for (index, target) in report.targets.iter().enumerate() {
        let mut result = format!("Target {} · {}", target.target, outcome(target.outcome));
        if let RunOutcome::Failed {
            exit_code: Some(code),
        } = target.outcome
        {
            result.push_str(&format!(" · Exit status {code}"));
        }
        details = details.push(rule::horizontal(1)).push(
            column![
                text(format!("{}. {}", index + 1, target.label))
                    .size(14)
                    .wrapping(text::Wrapping::WordOrGlyph),
                text(result).size(12),
                text(&target.detail).wrapping(text::Wrapping::WordOrGlyph),
            ]
            .spacing(4),
        );
    }
    if let Some(comparison) = &report.comparison {
        details = details.push(rule::horizontal(1)).push(
            column![
                text("AI comparison").size(14),
                text(comparison)
                    .font(iced::Font::MONOSPACE)
                    .size(13)
                    .wrapping(text::Wrapping::WordOrGlyph),
            ]
            .spacing(4),
        );
    }

    details.into()
}
