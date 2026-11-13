//! Plan, consent and selective publication surfaces for a snippet split.

use std::fmt;

use iced::advanced::text::Wrapping;
use iced::widget::{Column, Row, button, checkbox, column, pick_list, row, text};
use iced::{Alignment, Element, Font, Length};

use crate::ai_manager::ProfileRef;
use crate::snippet_analysis::model::modularization::Job;

use super::{Message, ProfileChoice, WindowMessage, notice, wrap};

#[derive(Clone)]
pub enum Action {
    Profile(ProfileRef),
    Plan,
    Generate,
    Review,
    Confirm,
    ConfirmOutbound(u64),
    Back,
    Page(bool),
    File(bool),
    Include(usize, bool),
    Publish,
    RecoverPublication,
    Discard,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum ProviderKind {
    Local,
    Https,
    Cli,
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct Provider {
    pub(crate) reference: ProfileRef,
    pub(crate) name: String,
    pub(crate) kind: ProviderKind,
}

impl fmt::Display for Provider {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(&self.name)?;
        match self.kind {
            ProviderKind::Local => Ok(()),
            ProviderKind::Https => output.write_str(" · HTTPS"),
            ProviderKind::Cli => output.write_str(" · CLI"),
        }
    }
}

pub(crate) struct Controls {
    pub(crate) eligible: bool,
    pub(crate) providers: Vec<Provider>,
    pub(crate) selected: Option<ProfileRef>,
    pub(crate) locked: bool,
}

#[derive(Clone, Copy)]
pub(crate) enum OutboundDestination {
    Https,
    Cli,
}

#[derive(Clone, Copy)]
pub(crate) struct OutboundPreview<'a> {
    pub(crate) title: &'static str,
    pub(crate) profile: &'a ProfileChoice,
    pub(crate) module: Option<&'a str>,
    pub(crate) masked_source: bool,
    pub(crate) page: usize,
    pub(crate) pages: usize,
    pub(crate) prompt: &'a str,
    pub(crate) destination: OutboundDestination,
    pub(crate) approval: u64,
}

#[derive(Clone, Copy)]
pub(crate) enum Phase<'a> {
    Preview {
        title: &'static str,
        profile: &'a ProfileChoice,
        page: usize,
        pages: usize,
        prompt: &'a str,
    },
    OutboundPreview(OutboundPreview<'a>),
    Review {
        job: &'a Job,
        file: usize,
        included: bool,
        count: usize,
        original: Option<&'a str>,
        proposed: &'a str,
        diagnostics: &'a [String],
        destination: &'a str,
        page: usize,
        pages: usize,
    },
}

pub(super) fn controls<'a>(job: Option<&'a Job>, controls: &Controls) -> Column<'a, WindowMessage> {
    let mut body = column![text("Modularization").size(15)].spacing(6);
    let complete = job
        .and_then(|job| job.generation.as_ref())
        .is_some_and(|generation| generation.progress.ready());
    if !complete && job.is_none_or(|job| !job.published && job.publication.is_none()) {
        let selected = controls
            .providers
            .iter()
            .find(|provider| Some(&provider.reference) == controls.selected.as_ref());
        if controls.locked {
            body = body.push(text(selected.map_or_else(
                || "The split plan's AI profile is unavailable.".into(),
                |provider| format!("AI profile · {provider}"),
            )));
        } else if !controls.providers.is_empty() {
            body = body.push(
                row![
                    text("AI profile").size(13),
                    pick_list(controls.providers.clone(), selected.cloned(), |provider| {
                        msg(Action::Profile(provider.reference))
                    })
                    .placeholder("Choose a profile"),
                ]
                .align_y(Alignment::Center)
                .spacing(6)
                .wrap(),
            );
        }
    }
    let Some(job) = job else {
        return body.push(
            button("Plan a split…").on_press_maybe(controls.eligible.then(|| msg(Action::Plan))),
        );
    };
    body = body.push(
        text(&job.plan.rationale)
            .size(13)
            .wrapping(Wrapping::WordOrGlyph),
    );
    if job.published {
        return body.push(
            text(format!(
                "Published {} selected files.",
                job.publication
                    .as_ref()
                    .map_or(0, |publication| publication.count())
            ))
            .size(13),
        );
    }
    for file in &job.plan.files {
        body = body.push(
            text(format!(
                "{} · {}{}",
                file.path,
                if file.entry_point {
                    "Entry point · "
                } else {
                    ""
                },
                file.purpose
            ))
            .size(13)
            .wrapping(Wrapping::WordOrGlyph),
        );
    }
    let planned_action = if job.publication.is_some() {
        button("Recover publication…").on_press(msg(Action::RecoverPublication))
    } else if complete {
        button("Review proposed files…").on_press(msg(Action::Review))
    } else if job.plan.recommended {
        button(if job.generation.is_some() {
            "Review remaining generation…"
        } else {
            "Review module generation…"
        })
        .on_press_maybe(controls.eligible.then(|| msg(Action::Generate)))
    } else {
        button("Plan again…").on_press_maybe(controls.eligible.then(|| msg(Action::Plan)))
    };
    body.push(
        row![
            planned_action,
            job.publication
                .is_none()
                .then(|| button("Discard split plan").on_press(msg(Action::Discard))),
        ]
        .spacing(8)
        .wrap(),
    )
}

pub(super) fn view<'a>(
    phase: Phase<'a>,
    status: Option<&'a str>,
) -> (Column<'a, WindowMessage>, Row<'a, WindowMessage>) {
    match phase {
        Phase::Preview {
            title,
            profile,
            page,
            pages,
            prompt,
        } => (
            column![
                text(title).size(17),
                text(format!(
                    "{} · {} · {}",
                    profile.name, profile.model, profile.endpoint
                ))
                .wrapping(Wrapping::WordOrGlyph),
                status.map(notice),
                pager(page, pages),
                text(prompt)
                    .size(13)
                    .font(Font::MONOSPACE)
                    .wrapping(Wrapping::WordOrGlyph),
            ]
            .spacing(8),
            row![
                button("Back to report").on_press(msg(Action::Back)),
                button("Approve and continue").on_press(msg(Action::Confirm))
            ],
        ),
        Phase::OutboundPreview(preview) => outbound_preview(preview, status),
        Phase::Review {
            job,
            file,
            included,
            count,
            original,
            proposed,
            diagnostics,
            destination,
            page,
            pages,
        } => {
            let module = &job.plan.files[file];
            let mut body = column![
                text("Review generated files").size(17),
                text(format!("Destination folder · {destination}"))
                    .size(13)
                    .wrapping(Wrapping::WordOrGlyph),
                status.map(notice),
                row![
                    button("Previous file")
                        .on_press_maybe((file > 0).then(|| msg(Action::File(false)))),
                    text(format!(
                        "{} · {} of {}{}",
                        module.path,
                        file + 1,
                        job.plan.files.len(),
                        if module.entry_point {
                            " · Entry point"
                        } else {
                            ""
                        }
                    ))
                    .wrapping(Wrapping::WordOrGlyph),
                    button("Next file").on_press_maybe(
                        (file + 1 < job.plan.files.len()).then(|| msg(Action::File(true)))
                    ),
                ]
                .spacing(8)
                .align_y(Alignment::Center)
                .wrap(),
                checkbox(included)
                    .label("Include this file in publication")
                    .on_toggle(move |enabled| msg(Action::Include(file, enabled))),
                pager(page, pages),
            ]
            .spacing(8);
            for diagnostic in diagnostics {
                body = body.push(notice(diagnostic));
            }
            body = body.push(
                row![
                    column![
                        text("Original").size(14),
                        text(original.unwrap_or("New file"))
                            .size(13)
                            .font(Font::MONOSPACE)
                            .wrapping(Wrapping::WordOrGlyph)
                    ]
                    .width(Length::FillPortion(1))
                    .spacing(4),
                    column![
                        text("Proposed").size(14),
                        text(proposed)
                            .size(13)
                            .font(Font::MONOSPACE)
                            .wrapping(Wrapping::WordOrGlyph)
                    ]
                    .width(Length::FillPortion(1))
                    .spacing(4),
                ]
                .spacing(12),
            );
            (
                body,
                row![
                    button("Review later").on_press(msg(Action::Back)),
                    button(text(format!("Publish {count} selected file(s)")))
                        .on_press_maybe((count > 0).then(|| msg(Action::Publish))),
                ],
            )
        }
    }
}

fn outbound_preview<'a>(
    preview: OutboundPreview<'a>,
    status: Option<&'a str>,
) -> (Column<'a, WindowMessage>, Row<'a, WindowMessage>) {
    let body = column![
        text(preview.title).size(17),
        text(format!(
            "{} · {} · {}",
            preview.profile.name, preview.profile.model, preview.profile.endpoint
        ))
        .wrapping(Wrapping::WordOrGlyph),
        preview.module.map(|module| text(format!("Module · {module}"))
            .wrapping(Wrapping::WordOrGlyph)),
        preview.masked_source.then(|| notice(
            "The request withholds masked source values. Review the generated files for any manual adjustments before publishing."
        )),
        status.map(notice),
    ]
    .spacing(8);
    let (body, approve) = match preview.destination {
        OutboundDestination::Https => (body, "Send reviewed HTTPS request"),
        OutboundDestination::Cli => (
            body.push(
                text("The approved program can access files, accounts, or the network.").size(13),
            ),
            "Run reviewed CLI request",
        ),
    };
    (
        body.push(pager(preview.page, preview.pages)).push(
            text(preview.prompt)
                .size(13)
                .font(Font::MONOSPACE)
                .wrapping(Wrapping::WordOrGlyph),
        ),
        row![
            button("Back to report").on_press(msg(Action::Back)),
            button(approve).on_press(msg(Action::ConfirmOutbound(preview.approval))),
        ],
    )
}

fn pager(page: usize, pages: usize) -> Element<'static, WindowMessage> {
    row![
        button("Previous page").on_press_maybe((page > 0).then(|| msg(Action::Page(false)))),
        text(format!("Page {} of {pages}", page + 1)).size(13),
        button("Next page").on_press_maybe((page + 1 < pages).then(|| msg(Action::Page(true)))),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .wrap()
    .into()
}

pub(super) fn msg(action: Action) -> WindowMessage {
    wrap(Message::Modularization(action))
}
