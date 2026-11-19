//! Selection and approval controls for applying a saved analysis.

use iced::advanced::text::Wrapping;
use iced::widget::{Column, button, checkbox, column, container, pick_list, row, scrollable, text};
use iced::{Alignment, Element, Font, Length};

use crate::snippet_analysis::model::{ApplyRun, ApplyStatus, Record, Selection};
use crate::snippet_window::ai_assist::migration;
use crate::snippet_window::languages::mix::HostFormat;
use crate::snippets::SnippetId;

use super::hardening;
use super::{Message, ProfileChoice, WindowMessage, action, notice, wrap};
use crate::snippet_window::compare;
use crate::snippet_window::diff_summary;

#[derive(Clone)]
pub enum Action {
    Finding(SnippetId, String, bool),
    Dependency(SnippetId, String, bool),
    AllFindings(SnippetId, bool),
    Hardening(SnippetId, hardening::Edit),
    MigrationLanguage(SnippetId, Option<migration::Language>),
    MigrationPlatform(SnippetId, Option<HostFormat>),
    Plan(SnippetId, bool),
    Recover(SnippetId),
    DiscardRecovery(SnippetId),
    Resume,
    ReviewPartial,
    PreviewPage(bool),
    Approve,
    Back,
    Open(SnippetId, SnippetId),
    RevealReason(SnippetId, Option<usize>),
    Diff(compare::Message),
    SummaryDivider(SnippetId, diff_summary::Event),
    ExactPage(bool),
    ToggleExact,
    FolderFile(bool),
    FolderInclude(SnippetId, bool),
    Accept,
    Reject,
    Restore,
}

pub(crate) struct Controls<'a> {
    pub(crate) selection: Option<&'a Selection>,
    pub(crate) can_plan: bool,
    pub(crate) hardening: Option<hardening::Controls<'a>>,
    pub(crate) migration: Option<Migration>,
}

pub(crate) struct Migration {
    pub(crate) languages: Vec<migration::Language>,
    pub(crate) platforms: Vec<HostFormat>,
    pub(crate) plan: Option<migration::Plan>,
}

impl Migration {
    fn view<'a>(&self, id: SnippetId) -> Column<'a, WindowMessage> {
        let language = self.plan.and_then(migration::Plan::language);
        let platform = self.plan.and_then(migration::Plan::destination);
        column![
            (!self.platforms.is_empty()).then(|| row![
                pick_list(self.platforms.clone(), platform, move |value| {
                    msg(Action::MigrationPlatform(id, Some(value)))
                })
                .placeholder("Keep current platform"),
                platform.map(|_| control("Clear platform", Action::MigrationPlatform(id, None))),
            ]
            .spacing(6)),
            (!self.languages.is_empty()).then(|| row![
                pick_list(self.languages.clone(), language, move |value| {
                    msg(Action::MigrationLanguage(id, Some(value)))
                })
                .placeholder("Keep current programming language"),
                language.map(|_| control("Clear language", Action::MigrationLanguage(id, None))),
            ]
            .spacing(6))
        ]
        .spacing(6)
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Preview<'a> {
    pub(crate) profile: &'a ProfileChoice,
    pub(crate) prompt: &'a str,
    pub(crate) page: usize,
    pub(crate) pages: usize,
    pub(crate) stale: bool,
    pub(crate) stages: usize,
    pub(crate) completed: usize,
    pub(crate) retrying: bool,
    pub(crate) project_files: Option<usize>,
}

#[derive(Clone, Copy)]
pub(crate) struct Review<'a> {
    pub(crate) pre_id: SnippetId,
    pub(crate) run: &'a ApplyRun,
    pub(crate) comparison: &'a compare::View,
    pub(crate) locations: &'a [Option<compare::ChangeLocation>],
    pub(crate) focused_reason: Option<usize>,
    pub(crate) exact: Option<Exact<'a>>,
    pub(crate) can_install: bool,
    pub(crate) can_replan: bool,
    pub(crate) summary_drag: Option<u16>,
}

#[derive(Clone, Copy)]
pub(crate) struct Exact<'a> {
    pub(crate) before: &'a str,
    pub(crate) after: &'a str,
    pub(crate) page: usize,
    pub(crate) pages: usize,
}

#[derive(Clone, Copy)]
pub(crate) struct FolderReview<'a> {
    pub(crate) change: Review<'a>,
    pub(crate) id: SnippetId,
    pub(crate) path: &'a str,
    pub(crate) index: usize,
    pub(crate) total: usize,
    pub(crate) included: bool,
    pub(crate) selected: usize,
    pub(crate) failures: &'a [(String, String)],
}

pub(super) fn controls<'a>(
    record: &'a Record,
    controls: &Controls<'a>,
    stale: bool,
    project: bool,
    represented_run: Option<SnippetId>,
) -> Column<'a, WindowMessage> {
    let selected = controls.selection;
    let has_findings = !record.analysis.improvements.is_empty();
    let has_dependencies = record
        .analysis
        .dependencies
        .iter()
        .any(|item| !item.suggestion.trim().is_empty());
    let can_select =
        selected.is_some() && (has_findings || has_dependencies || controls.hardening.is_some());
    let id = record.id;
    let mut body = column![].spacing(6);
    if selected.is_some() && record.apply_recovery.is_some() {
        body = body.push(
            row![
                control("Recover interrupted apply…", Action::Recover(id)),
                control("Discard checkpoint", Action::DiscardRecovery(id)),
            ]
            .spacing(6)
            .wrap(),
        );
    } else if can_select {
        if let Some(migration) = &controls.migration {
            body = body.push(migration.view(id));
        }
        if let Some(hardening) = &controls.hardening {
            body = body.push(hardening::view(hardening, id));
        }
        let has_work = selected.is_some_and(|selection| {
            !selection.findings.is_empty() || !selection.dependencies.is_empty()
        }) || controls
            .hardening
            .as_ref()
            .is_some_and(|hardening| hardening.draft.has_work(hardening.input_supported));
        let all = selected.is_some_and(|selection| {
            record
                .analysis
                .improvements
                .iter()
                .all(|item| selection.findings.contains(&item.id))
        });
        body = body.push(
            row![
                has_findings.then(|| checkbox(all)
                    .label("Select all improvements")
                    .on_toggle(move |selected| msg(Action::AllFindings(id, selected)))),
                button(if stale && !project {
                    "Apply to current source…"
                } else {
                    "Apply selected…"
                })
                .on_press_maybe(
                    (controls.can_plan && has_work).then(|| msg(Action::Plan(id, stale)))
                ),
            ]
            .spacing(6)
            .align_y(Alignment::Center)
            .wrap(),
        );
        if !controls.can_plan {
            body = body.push(text(if project && stale {
                "Run the project analysis again before applying selections."
            } else if project {
                "Enable the report’s AI profile to apply project selections."
            } else {
                "Open the editable snippet and enable the report’s AI profile to apply selections."
            }).size(13));
        }
    }
    for run in record
        .apply_runs
        .iter()
        .filter(|run| Some(run.id) != represented_run)
    {
        body = body.push(
            row![
                text(format!(
                    "{} · {} selected{}",
                    run.status.label(),
                    run.selected.len(),
                    if run.partial.is_some() {
                        " · Partial proposal"
                    } else {
                        ""
                    },
                ))
                .size(13),
                control("Review changes…", Action::Open(id, run.id)),
            ]
            .spacing(6)
            .align_y(Alignment::Center)
            .wrap(),
        );
    }
    body
}

pub(super) fn preview_body(preview: Preview<'_>) -> Column<'_, WindowMessage> {
    let explanation = if preview.completed > 0 {
        format!(
            "The first {} of {} stages are complete. This sends the remaining items and the last validated source to the report’s AI profile. You can review the complete proposal before changing the editor.",
            preview.completed, preview.stages,
        )
    } else if let Some(count) = preview.project_files {
        format!(
            "This sends the selected items and project context to the report’s AI profile for {count} file(s) in {} stage(s). Review and select the proposed files before saving.",
            preview.stages,
        )
    } else {
        format!(
            "This sends the selected items and source to the report’s AI profile in {} stage(s). You can review the complete proposal before changing the editor.",
            preview.stages,
        )
    };
    column![
        text("Review the AI request").size(17),
        text(format!(
            "{} · {} · {}",
            preview.profile.name, preview.profile.model, preview.profile.endpoint,
        ))
        .wrapping(Wrapping::WordOrGlyph),
        text(explanation).size(13).wrapping(Wrapping::WordOrGlyph),
        preview.profile.remote.then(|| notice("This sends source code over HTTPS. A request or proposal containing protected text stops the run.")),
        preview.stale.then(|| notice("You selected the current source. The saved recommendations were made for an earlier revision.")),
        text(preview.prompt).font(Font::MONOSPACE).size(13).wrapping(Wrapping::WordOrGlyph),
    ]
    .spacing(8)
}

pub(super) fn review_body<'a>(
    review: Review<'a>,
    status: Option<&'a str>,
    font_size: u8,
    summary_divider: Option<u16>,
) -> Element<'a, WindowMessage> {
    let run = review.run;
    let proposed = run.status == ApplyStatus::Proposed;
    let restore = run.status == ApplyStatus::Accepted;
    review_content(review, status, font_size, summary_divider)
        .push((proposed && run.has_review_source() && !review.can_install).then(|| notice("The exact original revision must be active in an editable snippet before accepting this proposal.")))
        .push(row![
            control(if proposed { "Review later" } else { "Back to report" }, Action::Back),
            proposed.then(|| control("Reject", Action::Reject).style(button::danger)),
            (proposed && !review.can_install && review.can_replan)
                .then(|| control("Propose for current source…", Action::Plan(review.pre_id, true))),
            proposed.then(|| button(if run.partial.is_some() { "Accept completed changes" } else { "Accept changes" })
                .on_press_maybe(review.can_install.then(|| msg(Action::Accept)))),
            (restore && review.can_install).then(|| control("Restore accepted draft", Action::Restore)),
        ].spacing(8).wrap())
        .into()
}

pub(super) fn folder_review_body<'a>(
    review: FolderReview<'a>,
    status: Option<&'a str>,
    font_size: u8,
    summary_divider: Option<u16>,
) -> Element<'a, WindowMessage> {
    let failures = (!review.failures.is_empty()).then(|| {
        scrollable(
            column(review.failures.iter().map(|(path, error)| {
                text(format!("{path} · {error}"))
                    .size(13)
                    .wrapping(Wrapping::WordOrGlyph)
                    .into()
            }))
            .spacing(4),
        )
        .height(Length::Fixed(60.0))
    });
    column![
        row![
            button("Previous file")
                .on_press_maybe((review.index > 0).then(|| msg(Action::FolderFile(false)))),
            text(format!(
                "{} · File {} of {}",
                review.path,
                review.index + 1,
                review.total
            ))
            .wrapping(Wrapping::WordOrGlyph),
            button("Next file").on_press_maybe(
                (review.index + 1 < review.total).then(|| msg(Action::FolderFile(true)))
            ),
            checkbox(review.included)
                .label("Include this file")
                .on_toggle(move |included| msg(Action::FolderInclude(review.id, included))),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .wrap(),
        (!review.failures.is_empty())
            .then(|| notice("Some files could not be proposed. They will stay as saved.")),
        failures,
        review_content(review.change, status, font_size, summary_divider),
        row![
            control("Discard proposals", Action::Back),
            button(text(format!("Save {} selected file(s)", review.selected)))
                .on_press_maybe((review.selected > 0).then(|| msg(Action::Accept))),
        ]
        .spacing(8)
        .wrap(),
    ]
    .spacing(8)
    .height(Length::Fill)
    .into()
}

fn review_content<'a>(
    review: Review<'a>,
    status: Option<&'a str>,
    font_size: u8,
    summary_divider: Option<u16>,
) -> Column<'a, WindowMessage> {
    let run = review.run;
    let summary = move || {
        let reasons = column(run.reasons.iter().zip(review.locations).enumerate().map(
            |(index, (reason, location))| {
                let jump = location.map(|location| {
                    let label = location.label();
                    button(text(label).size(12))
                        .on_press(msg(Action::RevealReason(run.id, Some(index))))
                        .style(if review.focused_reason == Some(index) {
                            button::primary
                        } else {
                            button::secondary
                        })
                });
                row![
                    text(format!("{} · {}", reason.id, reason.reason))
                        .size(13)
                        .wrapping(Wrapping::WordOrGlyph),
                    jump,
                ]
                .spacing(6)
                .align_y(Alignment::Center)
                .wrap()
                .into()
            },
        ))
        .spacing(4);
        let migration = run.migration.map(|migration| {
            column![
                migration
                    .destination()
                    .map(|target| text(format!("Proposed platform · {target}")).size(13)),
                migration
                    .language()
                    .map(|target| text(format!("Proposed language · {target}")).size(13)),
                column(
                    run.migration_notes
                        .iter()
                        .map(|note| text(note).size(13).into())
                )
                .spacing(4),
            ]
            .spacing(4)
        });
        column![migration, reasons].spacing(6).into()
    };
    let source = move || {
        review.exact.map_or_else(
            || {
                review
                    .comparison
                    .review_body_with_font(Some(font_size))
                    .map(|message| msg(Action::Diff(message)))
            },
            |page| exact(page, font_size),
        )
    };
    let has_summary = !run.reasons.is_empty() || run.migration.is_some();
    let content = match (has_summary, run.has_review_source()) {
        (true, true) => {
            let lines = run.reasons.len().saturating_add(run.migration_notes.len());
            let estimate = 28.0 + lines.min(24) as f32 * 20.0;
            Some(diff_summary::view(
                review.summary_drag.or(summary_divider),
                estimate,
                summary,
                source,
                move |event| msg(Action::SummaryDivider(run.id, event)),
            ))
        }
        (true, false) => Some(scrollable(summary()).height(Length::Fill).into()),
        (false, true) => Some(source()),
        (false, false) => None,
    };
    column![
        row![
            text(run.status.label()).size(17),
            run.has_review_source().then(|| control(
                if review.exact.is_some() {
                    "Aligned diff"
                } else {
                    "Exact source"
                },
                Action::ToggleExact
            )),
            run.has_review_source()
                .then(|| super::diff_font_controls(font_size)),
            review
                .focused_reason
                .map(|_| control("Show all changes", Action::RevealReason(run.id, None))),
        ]
        .align_y(Alignment::Center)
        .spacing(8)
        .wrap(),
        run.partial
            .map(|partial| text(format!("Partial proposal · {}", partial.progress())).size(13)),
        status.map(notice),
        (!run.has_review_source()).then(|| notice(
            "The analysis retention limit omitted source needed to review this change."
        )),
        content,
    ]
    .spacing(8)
    .height(Length::Fill)
}

fn exact(page: Exact<'_>, font_size: u8) -> Element<'_, WindowMessage> {
    column![
        row![
            exact_side("Original", page.before, font_size),
            exact_side("Proposed", page.after, font_size),
        ]
        .height(Length::Fill)
        .spacing(12),
        row![
            button("Previous")
                .on_press_maybe((page.page > 0).then(|| msg(Action::ExactPage(false)))),
            text(format!("Page {} of {}", page.page + 1, page.pages)).size(13),
            button("Next")
                .on_press_maybe((page.page + 1 < page.pages).then(|| msg(Action::ExactPage(true)))),
        ]
        .align_y(Alignment::Center)
        .spacing(8),
    ]
    .spacing(6)
    .height(Length::Fill)
    .into()
}

fn exact_side<'a>(
    title: &'static str,
    source: &'a str,
    font_size: u8,
) -> Column<'a, WindowMessage> {
    let mut escaped = String::new();
    for character in source.chars() {
        escaped.extend(character.escape_debug());
        if character == '\n' {
            escaped.push('\n');
        }
    }
    column![
        text(title).size(14),
        scrollable(
            container(
                text(escaped)
                    .font(Font::MONOSPACE)
                    .size(u32::from(font_size))
                    .wrapping(Wrapping::WordOrGlyph)
            )
            .padding(6)
        )
        .height(Length::Fill)
        .width(Length::Fill),
    ]
    .width(Length::FillPortion(1))
    .spacing(5)
}

pub(super) fn control(
    label: &'static str,
    action_message: Action,
) -> iced::widget::Button<'static, WindowMessage> {
    action(label, Message::Apply(action_message))
}

pub(super) fn msg(action: Action) -> WindowMessage {
    wrap(Message::Apply(action))
}
