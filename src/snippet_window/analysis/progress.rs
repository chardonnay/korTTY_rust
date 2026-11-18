//! A shared live or saved apply summary for inline and docked presentation.

use iced::advanced::text::Wrapping;
use iced::widget::{Column, button, column, container, row, scrollable, text};
use iced::{Element, Length};

use crate::snippet_analysis::model::{ApplyRun, java_history};
use crate::snippet_analysis::request::apply::Progress;
use crate::snippets::SnippetId;

use super::{Message, WindowMessage, apply, wrap};

#[derive(Clone, Copy)]
pub(crate) enum Content<'a> {
    Live {
        status: &'a str,
        update: Option<&'a Progress>,
    },
    Saved {
        record: SnippetId,
        run: &'a ApplyRun,
        review: bool,
    },
    Interrupted {
        completed: usize,
        total: usize,
    },
    Imported(&'a java_history::ApplyRun),
}

impl Content<'_> {
    pub(crate) fn follows_live(self, session_runs: &std::collections::HashSet<SnippetId>) -> bool {
        match self {
            Self::Live { .. } => true,
            Self::Saved { run, .. } => session_runs.contains(&run.id),
            Self::Interrupted { .. } | Self::Imported(_) => false,
        }
    }

    pub(super) fn review_run(self) -> Option<SnippetId> {
        match self {
            Self::Saved {
                run, review: true, ..
            } => Some(run.id),
            _ => None,
        }
    }
}

pub(crate) fn dock_width(space: f32) -> Option<f32> {
    const MIN_DOCK_WIDTH: f32 = 300.0;
    const DEFAULT_DOCK_WIDTH: f32 = 400.0;
    (space.is_finite() && space >= MIN_DOCK_WIDTH).then(|| space.min(DEFAULT_DOCK_WIDTH))
}

fn summary(content: Content<'_>) -> Column<'_, WindowMessage> {
    let body = match content {
        Content::Live {
            status,
            update: None,
        } => column![text(status)],
        Content::Live {
            update: Some(progress),
            ..
        } => {
            let file = progress.file.map_or_else(String::new, |(file, total)| {
                format!("File {file} of {total} · ")
            });
            let status = if progress.repair {
                "Repairing response"
            } else {
                "Generating proposal"
            };
            column![
                text(format!(
                    "{file}Stage {} of {} · {status} · {} answer bytes received",
                    progress.stage, progress.stages, progress.answer_bytes,
                ))
                .size(14),
                (!progress.reason.is_empty()).then(|| column![
                    text("Provisional change reason").size(13),
                    text(&progress.reason).wrapping(Wrapping::WordOrGlyph),
                ]
                .spacing(4)),
            ]
        }
        Content::Saved {
            record,
            run,
            review,
        } => column![
            text(format!(
                "{} · {} selected",
                run.status.label(),
                run.selected.len()
            ))
            .size(14),
            run.partial.map(|partial| text(partial.progress()).size(13)),
            review.then(|| apply::control("Review changes…", apply::Action::Open(record, run.id))),
        ],
        Content::Interrupted { completed, total } => column![
            text("Interrupted apply"),
            text(format!("{completed} of {total} stages completed.")).size(13),
        ],
        Content::Imported(run) => column![
            text(run.outcome.label(run.saved_to_snippet_at)).size(14),
            (!run.summary.is_empty()).then(|| text(&run.summary).wrapping(Wrapping::WordOrGlyph)),
        ],
    };
    body.spacing(7).width(Length::Fill)
}

pub(super) fn inline(content: Content<'_>) -> Element<'_, WindowMessage> {
    container(column![text("Apply progress").size(15), summary(content)].spacing(8))
        .padding(9)
        .width(Length::Fill)
        .style(container::secondary)
        .into()
}

pub(super) fn dock(content: Content<'_>, width: f32) -> Element<'_, WindowMessage> {
    container(
        column![
            row![
                text("Apply progress").size(17).width(Length::Fill),
                button("Move into report").on_press(wrap(Message::ProgressDock(false))),
            ]
            .spacing(6)
            .wrap(),
            scrollable(summary(content))
                .width(Length::Fill)
                .height(Length::Fill),
        ]
        .spacing(10),
    )
    .width(width)
    .height(Length::Fill)
    .padding(10)
    .style(container::secondary)
    .into()
}
