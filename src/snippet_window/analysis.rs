//! Native review surfaces for snippet and folder analyses.

use std::fmt;

use iced::advanced::text::Wrapping;
use iced::widget::{
    Column, Row, button, checkbox, column, container, keyed_column, pick_list, row, rule,
    scrollable, svg, text, text_input,
};
use iced::{Alignment, Element, Font, Length, Theme};

use crate::ai_manager::{ProfileId, ProfileRef};
use crate::snippet_analysis::model::{
    Dependency, Finding, History, Purpose, Record, Subject, Verification,
};
use crate::snippets::SnippetId;

use super::Message as WindowMessage;

pub(crate) mod apply;
pub(crate) mod hardening;
pub(crate) mod modularization;

#[derive(Clone)]
pub enum Message {
    Tab(Subject),
    Editor,
    Close,
    Profile(ProfileRef),
    Instructions(String),
    Skill(u64, bool),
    Files(bool),
    CancelFiles,
    IncludeFile(SnippetId, bool),
    RequestStart,
    Verify(SnippetId),
    Apply(apply::Action),
    Modularization(modularization::Action),
    ToggleResolved,
    ConfirmStart,
    ConfirmCli(u64),
    CancelStart,
    KnowledgeStore(usize),
    RetrieveKnowledge,
    ClearKnowledge,
    ReviewPage(bool),
    Abort,
    SelectRecord(SnippetId),
    Pin(bool),
    RequestDiscard,
    Discard,
    CancelDiscard,
    Compare,
    Diagram,
    GenerateDiagram,
    AutoDiagram(bool),
    AutoModularization(bool),
    Export,
    Reports(crate::workspace::SnippetReportAction),
    Jump { file: Option<String>, line: usize },
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct ProfileChoice {
    pub(crate) id: ProfileId,
    pub(crate) reference: ProfileRef,
    pub(crate) name: String,
    pub(crate) model: String,
    pub(crate) endpoint: String,
    pub(crate) remote: bool,
    pub(crate) cli: bool,
}

impl fmt::Display for ProfileChoice {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(&self.name)?;
        if self.cli {
            output.write_str(" · CLI")?;
        } else if self.remote {
            output.write_str(" · HTTPS")?;
        }
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) struct FileChoice {
    pub(crate) id: SnippetId,
    pub(crate) path: String,
    pub(crate) bytes: usize,
    pub(crate) included: bool,
}

#[derive(Clone, Copy)]
pub(crate) enum ReviewMode<'a> {
    Analysis {
        profile: &'a ProfileChoice,
        replacements: Option<usize>,
        automatic_diagram: bool,
        automatic_plan: bool,
        outbound_plan: bool,
        knowledge: Option<KnowledgePreview<'a>>,
    },
    Flowchart(&'a ProfileChoice),
    Verification {
        profile: &'a ProfileChoice,
        replacements: Option<usize>,
        applied_revision: bool,
        automatic_diagram: bool,
    },
}

impl ReviewMode<'_> {
    fn remote_masking(self) -> Option<usize> {
        match self {
            Self::Analysis { replacements, .. } | Self::Verification { replacements, .. } => {
                replacements
            }
            Self::Flowchart(_) => None,
        }
    }

    fn cli(self) -> bool {
        match self {
            Self::Analysis { profile, .. } | Self::Verification { profile, .. } => profile.cli,
            Self::Flowchart(_) => false,
        }
    }
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct KnowledgeChoice {
    pub(crate) index: usize,
    pub(crate) name: String,
}

impl fmt::Display for KnowledgeChoice {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(&self.name)
    }
}

#[derive(Clone, Copy)]
pub(crate) struct KnowledgePreview<'a> {
    pub(crate) choices: &'a [KnowledgeChoice],
    pub(crate) selected: Option<&'a KnowledgeChoice>,
    pub(crate) route: Option<(&'a str, &'a str)>,
    pub(crate) qdrant_destination: Option<&'a str>,
    pub(crate) query: &'a str,
    pub(crate) running: bool,
    pub(crate) ready: bool,
}

#[derive(Clone, Copy, Default)]
pub(crate) enum Phase<'a> {
    #[default]
    Ready,
    Files,
    ConfirmStart {
        preview: &'a str,
        part: &'a str,
        /// Index within the complete request preview.
        page: usize,
        pages: usize,
        mode: ReviewMode<'a>,
        cli_approval: Option<u64>,
    },
    Running {
        status: &'a str,
    },
    ApplyPreview(apply::Preview<'a>),
    RecoverApply {
        completed: usize,
        total: usize,
        discard: Option<SnippetId>,
    },
    ReviewApply(apply::Review<'a>),
    ReviewFolderApply(apply::FolderReview<'a>),
    Modularization(modularization::Phase<'a>),
    SavingApply(&'static str),
    ConfirmDiscard,
}

pub(crate) struct View<'a> {
    pub(crate) subject: Subject,
    pub(crate) name: &'a str,
    pub(crate) profiles: &'a [ProfileChoice],
    pub(crate) cli_allowed: bool,
    pub(crate) profile: Option<ProfileId>,
    pub(crate) instructions: &'a str,
    pub(crate) skills: &'a [(u64, String)],
    pub(crate) selected_skills: &'a [u64],
    pub(crate) files: &'a [FileChoice],
    pub(crate) estimated_tokens: usize,
    pub(crate) history: Option<&'a History>,
    pub(crate) record: Option<SnippetId>,
    pub(crate) phase: Phase<'a>,
    pub(crate) live: Option<LivePreview<'a>>,
    pub(crate) apply_live: Option<&'a crate::snippet_analysis::request::apply::Progress>,
    pub(crate) stale: bool,
    pub(crate) status: Option<&'a str>,
    pub(crate) can_export: bool,
    pub(crate) can_compare: bool,
    pub(crate) can_jump: bool,
    pub(crate) verify_run: Option<SnippetId>,
    pub(crate) apply: apply::Controls<'a>,
    pub(crate) show_resolved: bool,
    pub(crate) auto_diagram: bool,
    pub(crate) auto_plan: bool,
    pub(crate) diagram_preview: Option<&'a svg::Handle>,
    pub(crate) can_generate_diagram: bool,
    pub(crate) modularization: modularization::Controls,
    pub(crate) scroll: iced::widget::Id,
}

pub(crate) struct LivePreview<'a> {
    pub(crate) answer_bytes: usize,
    pub(crate) summary: &'a str,
    pub(crate) reasoning: &'a str,
}

#[derive(Clone, Eq, PartialEq)]
struct HistoryChoice {
    id: SnippetId,
    label: String,
}

impl fmt::Display for HistoryChoice {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(&self.label)
    }
}

pub(crate) fn view<'a>(state: &View<'a>) -> Element<'a, WindowMessage> {
    let project = matches!(state.subject, Subject::Folder(_));
    let history = state
        .history
        .map_or(&[][..], |history| history.records.as_slice());
    let record = state
        .record
        .and_then(|id| history.iter().find(|record| record.id == id))
        .or_else(|| history.first());
    let profile = state
        .profiles
        .iter()
        .find(|profile| Some(profile.id) == state.profile);
    let included = state.files.iter().filter(|file| file.included).count();
    let can_start = profile.is_some() && (!project || included > 0);
    let running = matches!(state.phase, Phase::Running { .. } | Phase::SavingApply(_));
    let title = if project {
        "Project analysis"
    } else {
        "Code analysis"
    };

    let header = row![
        text(title).size(20),
        button("Editor").on_press_maybe(
            (!matches!(state.phase, Phase::SavingApply(_))).then_some(wrap(Message::Editor))
        ),
        button("Close analysis").on_press_maybe((!running).then_some(wrap(Message::Close))),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .wrap();
    let subject = column![
        text(state.name).size(16).wrapping(Wrapping::WordOrGlyph),
        project.then(|| {
            text(format!(
                "{included} files · about {} tokens · {} excluded",
                state.estimated_tokens,
                state.files.len().saturating_sub(included),
            ))
            .size(13)
        }),
    ]
    .spacing(3);

    let review = match state.phase {
        Phase::ReviewApply(review) => Some(apply::review_body(review, state.status)),
        Phase::ReviewFolderApply(review) => Some(apply::folder_review_body(review, state.status)),
        _ => None,
    };
    if let Some(review) = review {
        return column![header, rule::horizontal(1), subject, review,]
            .padding(12)
            .spacing(10)
            .width(Length::Fill)
            .height(Length::Fill)
            .clip(true)
            .into();
    }

    let (body, actions, phase_key) = match state.phase {
        Phase::Files => (
            file_chooser(state.files),
            row![
                action("Cancel", Message::CancelFiles),
                action("Done", Message::Files(false)),
            ],
            1,
        ),
        Phase::ConfirmStart { preview, mode, cli_approval, .. } => (
            confirmation(project, included, preview, mode),
            row![
                action("Back to report", Message::CancelStart),
                button(match mode {
                    mode if mode.cli() => "Run reviewed CLI analysis",
                    mode if mode.remote_masking().is_some() => "Send masked request to HTTPS provider",
                    ReviewMode::Analysis { knowledge: Some(knowledge), .. } if !knowledge.ready => {
                        "Start analysis without knowledge"
                    }
                    ReviewMode::Analysis { .. } => "Start analysis",
                    ReviewMode::Flowchart(_) => "Generate flowchart",
                    ReviewMode::Verification { .. } => "Verify applied changes",
                })
                .on_press_maybe(
                    ((!matches!(mode, ReviewMode::Analysis { .. }) || can_start)
                        && !matches!(mode, ReviewMode::Analysis { knowledge: Some(knowledge), .. } if knowledge.running))
                        .then_some(wrap(cli_approval.map_or(Message::ConfirmStart, Message::ConfirmCli)))
                ),
            ],
            2,
        ),
        Phase::ConfirmDiscard => (
            column![text("Discard the selected saved analysis?")],
            row![
                action("Cancel", Message::CancelDiscard),
                button("Discard")
                    .style(button::danger)
                    .on_press_maybe(record.is_some().then_some(wrap(Message::Discard))),
            ],
            3,
        ),
        Phase::ApplyPreview(preview) => (
            apply::preview_body(preview),
            row![
                apply::control(
                    if preview.retrying { "Back to recovery" } else { "Back to report" },
                    apply::Action::Back,
                ),
                apply::control("Approve and generate", apply::Action::Approve),
            ],
            4,
        ),
        Phase::RecoverApply { completed, total, discard } => (
            column![
                text("Recover the apply proposal").size(17),
                state.status.map(notice),
                text(format!("{completed} of {total} stages completed.")).size(13),
            ]
            .spacing(8),
            row![
                apply::control("Back to report", apply::Action::Back),
                (completed < total).then(|| apply::control(
                    if completed == 0 { "Review retry…" } else { "Review remaining request…" },
                    apply::Action::Resume,
                )),
                (completed > 0).then(|| apply::control("Review completed changes…", apply::Action::ReviewPartial)),
                discard.map(|pre| apply::control("Discard checkpoint", apply::Action::DiscardRecovery(pre))),
            ],
            5,
        ),
        Phase::ReviewApply(_) | Phase::ReviewFolderApply(_) => {
            unreachable!("apply review has its own scrolling surface")
        }
        Phase::Modularization(phase) => {
            let (body, actions) = modularization::view(phase, state.status);
            (body, actions, 6)
        }
        Phase::Ready | Phase::Running { .. } | Phase::SavingApply(_) => {
            let body = results(state, history, record, project);
            let mut actions = row![];
            if !running {
                actions = actions
                    .push(
                        button(if record.is_some() {
                            "Run again…"
                        } else {
                            "Analyze…"
                        })
                        .on_press_maybe(can_start.then_some(wrap(Message::RequestStart))),
                    )
                    .push(project.then(|| action("Files…", Message::Files(true))));
            } else if !matches!(state.phase, Phase::SavingApply(_)) {
                actions = actions.push(action("Stop", Message::Abort));
            }
            (body, actions, 0)
        }
    };
    let content: Element<'a, WindowMessage> =
        scrollable(column![subject, body].spacing(10).width(Length::Fill))
            .id(state.scroll.clone())
            .height(Length::Fill)
            .width(Length::Fill)
            .into();
    let (preview_page, pager) = match state.phase {
        Phase::ConfirmStart {
            part, page, pages, ..
        } => {
            let displayed = page.saturating_add(1);
            (
                page,
                Some(
                    row![
                        button("Previous")
                            .on_press_maybe((page > 0).then_some(wrap(Message::ReviewPage(false)))),
                        text(format!("{part} · Page {displayed} of {pages}"))
                            .size(13)
                            .wrapping(Wrapping::WordOrGlyph),
                        button("Next").on_press_maybe(
                            (displayed < pages).then_some(wrap(Message::ReviewPage(true)))
                        ),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center)
                    .wrap(),
                ),
            )
        }
        Phase::ApplyPreview(preview) => (
            preview.page,
            Some(
                row![
                    button("Previous").on_press_maybe(
                        (preview.page > 0).then(|| apply::msg(apply::Action::PreviewPage(false)))
                    ),
                    text(format!(
                        "Apply request · Page {} of {}",
                        preview.page + 1,
                        preview.pages
                    ))
                    .size(13),
                    button("Next").on_press_maybe(
                        (preview.page + 1 < preview.pages)
                            .then(|| apply::msg(apply::Action::PreviewPage(true)))
                    ),
                ]
                .spacing(8)
                .align_y(Alignment::Center)
                .wrap(),
            ),
        ),
        _ => (0, None),
    };
    let key = (phase_key, record.map(|record| record.id), preview_page);
    let progress = match state.phase {
        Phase::Running { status } | Phase::SavingApply(status) => Some(notice(status)),
        _ => None,
    };

    column![
        header,
        progress,
        rule::horizontal(1),
        keyed_column([(key, content)]).height(Length::Fill),
        pager,
        actions.spacing(8).wrap(),
    ]
    .padding(12)
    .spacing(10)
    .width(Length::Fill)
    .height(Length::Fill)
    .clip(true)
    .into()
}

fn results<'a>(
    state: &View<'a>,
    history: &'a [Record],
    record: Option<&'a Record>,
    project: bool,
) -> Column<'a, WindowMessage> {
    let mut body = column![].spacing(10);
    if let Some(status) = state.status {
        body = body.push(notice(status));
    }
    if let Some(preview) = &state.live {
        let heading = format!(
            "Live response · {} answer bytes received",
            preview.answer_bytes
        );
        body = body.push(
            container(
                column![
                    text(heading).size(14),
                    (!preview.summary.is_empty()).then(|| column![
                        text("Provisional summary").size(13),
                        text(preview.summary).wrapping(Wrapping::WordOrGlyph),
                    ]
                    .spacing(4)),
                    (!preview.reasoning.is_empty()).then(|| column![
                        text("Reasoning preview").size(13),
                        text(preview.reasoning).wrapping(Wrapping::WordOrGlyph),
                    ]
                    .spacing(4)),
                ]
                .spacing(7),
            )
            .padding(9)
            .width(Length::Fill)
            .style(container::secondary),
        );
    }
    if let Some(progress) = state.apply_live {
        let file = progress.file.map_or_else(String::new, |(file, total)| {
            format!("File {file} of {total} · ")
        });
        let status = if progress.repair {
            "Repairing response"
        } else {
            "Generating proposal"
        };
        let heading = format!(
            "{file}Stage {} of {} · {status} · {} answer bytes received",
            progress.stage, progress.stages, progress.answer_bytes
        );
        body = body.push(
            container(
                column![
                    text(heading).size(14),
                    (!progress.reason.is_empty()).then(|| column![
                        text("Provisional change reason").size(13),
                        text(&progress.reason).wrapping(Wrapping::WordOrGlyph),
                    ]
                    .spacing(4)),
                ]
                .spacing(7),
            )
            .padding(9)
            .width(Length::Fill)
            .style(container::secondary),
        );
    }
    if matches!(state.phase, Phase::Ready) {
        body = body
            .push(profile_picker(state))
            .push(
                text_input("Additional instructions (optional)", state.instructions)
                    .on_input(|instructions| wrap(Message::Instructions(instructions))),
            )
            .push(
                checkbox(state.auto_diagram)
                    .label("Generate a flowchart automatically for new analyses")
                    .on_toggle(|enabled| wrap(Message::AutoDiagram(enabled))),
            )
            .push(
                checkbox(state.auto_plan)
                    .label("Propose modularization for saved analyses")
                    .on_toggle(|enabled| wrap(Message::AutoModularization(enabled))),
            );
        if !state.skills.is_empty() {
            let mut skills = column![text("Skills for this request").size(14)].spacing(4);
            for (id, name) in state.skills {
                let id = *id;
                skills = skills.push(
                    checkbox(state.selected_skills.contains(&id))
                        .label(name)
                        .on_toggle(move |selected| wrap(Message::Skill(id, selected))),
                );
            }
            body = body.push(scrollable(skills).height(140));
        }
    }
    if project {
        body = body.push(
            text("Analyze the selected scripts together to find issues within and across files.")
                .size(13),
        );
    }
    if history.is_empty() {
        return body.push(text("No saved analysis yet."));
    }
    let Some(record) = record else {
        return body;
    };

    let choices: Vec<_> = history.iter().map(history_choice).collect();
    let selected = choices
        .iter()
        .find(|choice| choice.id == record.id)
        .cloned();
    let mut tools = row![
        record
            .diagram
            .is_some()
            .then(|| action("Enlarge flowchart", Message::Diagram)),
        button("Export report…").on_press_maybe(state.can_export.then_some(wrap(Message::Export))),
        button("Compare reports")
            .on_press_maybe(state.can_compare.then_some(wrap(Message::Compare))),
    ];
    if matches!(state.phase, Phase::Ready) {
        tools = tools
            .push(
                state
                    .verify_run
                    .map(|run| action("Verify…", Message::Verify(run))),
            )
            .push(action(
                if record.pinned { "Unpin" } else { "Pin" },
                Message::Pin(!record.pinned),
            ))
            .push(action("Discard…", Message::RequestDiscard));
    }
    body = body.push(
        column![
            text("Saved analyses").size(16),
            pick_list(choices, selected, |choice| wrap(Message::SelectRecord(
                choice.id
            )))
            .width(Length::Fill),
            tools.spacing(6).wrap(),
        ]
        .spacing(6),
    );
    if state.stale {
        body = body.push(notice(
            "The source changed after this analysis. Run it again for current findings.",
        ));
    }
    if let Some(current) = history.first()
        && record.id != current.id
    {
        body = body.push(
            row![
                text("Showing an older analysis").size(13),
                action("Show current", Message::SelectRecord(current.id)),
            ]
            .spacing(6)
            .align_y(Alignment::Center)
            .wrap(),
        );
    }
    if let Some(verification) = &record.verification {
        body = body.push(verification_panel(
            history,
            verification,
            state.show_resolved,
        ));
    }
    if matches!(state.phase, Phase::Ready) {
        body = body.push(apply::controls(record, &state.apply, state.stale, project));
        if record.purpose != Purpose::Verify {
            body = body.push(modularization::controls(
                record.modularization.as_deref(),
                &state.modularization,
            ));
        }
    }
    body.push(report(
        record,
        project,
        state.can_jump,
        state.apply.selection,
    ))
    .push(flowchart(state, record))
}

fn profile_picker<'a>(state: &View<'a>) -> Element<'a, WindowMessage> {
    let profiles: Vec<_> = state
        .profiles
        .iter()
        .filter(|profile| !profile.cli || state.cli_allowed)
        .cloned()
        .collect();
    if profiles.is_empty() {
        if state.profiles.iter().any(|profile| profile.cli) {
            return notice("Managed policy blocks the available CLI analysis profiles.");
        }
        return notice("Configure a coding profile in the AI Manager to start an analysis.");
    }
    let selected = state
        .profiles
        .iter()
        .find(|profile| Some(profile.id) == state.profile);
    row![
        text("AI profile").size(13),
        pick_list(profiles, selected, |profile| wrap(Message::Profile(
            profile.reference.clone()
        )))
        .placeholder("Choose a profile"),
    ]
    .align_y(Alignment::Center)
    .spacing(6)
    .wrap()
    .into()
}

fn file_chooser(files: &[FileChoice]) -> Column<'_, WindowMessage> {
    let mut body = column![
        text("Project files").size(17),
        text("Choose which files this analysis may send to the selected AI profile.").size(13),
    ]
    .spacing(8);
    for file in files {
        let id = file.id;
        body = body.push(
            row![
                checkbox(file.included)
                    .label(&file.path)
                    .text_wrapping(Wrapping::WordOrGlyph)
                    .width(Length::Fill)
                    .on_toggle(move |included| wrap(Message::IncludeFile(id, included))),
                text(format!("{} bytes", file.bytes)).size(12),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );
    }
    body
}

fn confirmation<'a>(
    project: bool,
    included: usize,
    preview: &'a str,
    mode: ReviewMode<'a>,
) -> Column<'a, WindowMessage> {
    let (title, description, profile, automatic_diagram) = match mode {
        ReviewMode::Analysis {
            profile,
            automatic_diagram,
            ..
        } => (
            "New analysis",
            if project {
                format!("Send the source of {included} selected files to this AI profile?")
            } else {
                "Send the current snippet source to this AI profile?".into()
            },
            profile,
            automatic_diagram,
        ),
        ReviewMode::Flowchart(profile) => (
            "Generate saved analysis flowchart",
            "Send the retained source of this analysis to its AI profile for a flowchart?".into(),
            profile,
            false,
        ),
        ReviewMode::Verification {
            profile,
            applied_revision,
            automatic_diagram,
            ..
        } => (
            "Verify applied changes",
            if applied_revision {
                "Analyze the accepted revision with the original AI profile.".into()
            } else {
                "The editor contains later edits. Compare this revision with the original analysis."
                    .into()
            },
            profile,
            automatic_diagram,
        ),
    };
    let mut body = column![text(title).size(17), text(description),].spacing(8);
    if automatic_diagram {
        body = body.push(notice("Approval also covers a separate source-only flowchart request after the analysis is saved."));
    }
    if matches!(
        mode,
        ReviewMode::Analysis {
            automatic_plan: true,
            ..
        }
    ) {
        body = body.push(notice("Approval also covers the split planning request shown in this preview. It runs after this analysis is saved."));
    }
    if matches!(
        mode,
        ReviewMode::Analysis {
            outbound_plan: true,
            ..
        }
    ) {
        body = body.push(notice(
            "After the analysis is saved, review a separate request to plan the split.",
        ));
    }
    if matches!(mode, ReviewMode::Flowchart(_)) || automatic_diagram {
        body = body.push(notice("If the flowchart answer is invalid, one correction may be requested. No additional source is included."));
    }
    body =
        body.push(text(format!("AI profile · {}", profile.name)).wrapping(Wrapping::WordOrGlyph));
    if !profile.model.is_empty() {
        body =
            body.push(text(format!("Model · {}", profile.model)).wrapping(Wrapping::WordOrGlyph));
    }
    if !profile.endpoint.is_empty() {
        body = body.push(
            text(format!("Destination · {}", profile.endpoint)).wrapping(Wrapping::WordOrGlyph),
        );
    }
    if let Some(replacements) = mode.remote_masking() {
        let description = if mode.cli() {
            "The approved program receives the displayed masked source and can access files, accounts, or the network. Review its command and all input sections."
        } else {
            "This HTTPS request sends the masked source and instructions outside this computer. Review both the system and user sections."
        };
        body = body.push(card(column![
            text(format!(
                "{description} Masking replacements · {replacements}."
            ))
            .size(13)
            .wrapping(Wrapping::WordOrGlyph)
        ]));
    }
    if let ReviewMode::Analysis {
        knowledge: Some(knowledge),
        ..
    } = mode
    {
        body = body.push(knowledge_review(knowledge));
    }
    body.push(text("Request preview").size(16)).push(
        text(preview)
            .font(Font::MONOSPACE)
            .size(12)
            .wrapping(Wrapping::WordOrGlyph),
    )
}

fn knowledge_review(preview: KnowledgePreview<'_>) -> Element<'_, WindowMessage> {
    let mut content = column![
        text("Optional knowledge retrieval").size(16),
        pick_list(preview.choices, preview.selected, |choice| wrap(
            Message::KnowledgeStore(choice.index)
        ))
        .placeholder("Select a knowledge store"),
    ]
    .spacing(6);
    if let Some((endpoint, model)) = preview.route {
        content = content
            .push(text(format!("Embedding endpoint · {endpoint} · model {model}")).wrapping(Wrapping::WordOrGlyph))
            .push(preview.qdrant_destination.map(|destination| text(format!("Qdrant destination · {destination}")).wrapping(Wrapping::WordOrGlyph)))
            .push(text(if preview.qdrant_destination.is_some() {
                "This query goes to the embedding service. Qdrant receives its vector and the index selection. Review the resulting excerpts before starting the analysis."
            } else {
                "Only this bounded query goes to the embedding endpoint. Review the resulting excerpts before starting the analysis."
            }).size(13))
            .push(text(preview.query).font(Font::MONOSPACE).size(12).wrapping(Wrapping::WordOrGlyph))
            .push(row![
                button("Approve and retrieve").on_press_maybe((!preview.running && !preview.ready)
                    .then_some(wrap(Message::RetrieveKnowledge))),
                action(if preview.running { "Cancel retrieval" } else { "Clear knowledge" }, Message::ClearKnowledge),
            ].spacing(8).wrap());
    }
    if preview.running {
        content = content.push(text("Retrieving knowledge…").size(13));
    } else if preview.ready {
        content = content
            .push(text("The analysis user prompt now includes the retrieved excerpts.").size(13));
    }
    content.into()
}

fn verification_panel<'a>(
    history: &'a [Record],
    verification: &'a Verification,
    show_resolved: bool,
) -> Element<'a, WindowMessage> {
    let resolved = &verification.resolved_previous_ids;
    let mut body = column![
        text(format!(
            "Verification · {} resolved · {} still open · {} new",
            resolved.len(),
            verification.persisting_current_to_previous.len(),
            verification.new_ids.len(),
        ))
        .size(16),
        text(if verification.matches_applied_revision {
            "Compared the accepted revision."
        } else {
            "Compared a later editor revision."
        })
        .size(13),
    ]
    .spacing(6);
    if history
        .iter()
        .any(|record| record.id == verification.previous_record_id)
    {
        body = body.push(action(
            "Show original analysis",
            Message::SelectRecord(verification.previous_record_id),
        ));
    }
    if !resolved.is_empty() {
        body = body.push(action(
            if show_resolved {
                "Hide resolved"
            } else {
                "Show resolved"
            },
            Message::ToggleResolved,
        ));
        if show_resolved {
            let previous = history
                .iter()
                .find(|record| record.id == verification.previous_record_id);
            for id in resolved {
                let title = previous.and_then(|record| {
                    record
                        .analysis
                        .improvements
                        .iter()
                        .find(|finding| finding.id == *id)
                        .map(|finding| finding.title.as_str())
                        .or_else(|| {
                            record
                                .analysis
                                .dependencies
                                .iter()
                                .find(|dependency| dependency.id == *id)
                                .map(|dependency| dependency.name.as_str())
                        })
                });
                body = body.push(
                    text(title.map_or_else(|| id.clone(), |title| format!("{id} · {title}")))
                        .size(13)
                        .wrapping(Wrapping::WordOrGlyph),
                );
            }
        }
    }
    card(body)
}

fn flowchart<'a>(state: &View<'a>, record: &'a Record) -> Column<'a, WindowMessage> {
    let ready = matches!(state.phase, Phase::Ready);
    let mut body = column![
        row![
            text("Flowchart").size(16),
            button(if record.diagram.is_some() {
                "Regenerate…"
            } else {
                "Generate…"
            })
            .on_press_maybe(
                (ready && state.can_generate_diagram).then_some(wrap(Message::GenerateDiagram))
            ),
        ]
        .spacing(6)
        .wrap(),
    ]
    .spacing(6);
    if let Some(diagram) = &record.diagram {
        if diagram.fallback {
            body = body.push(notice(
                crate::snippet_analysis::diagram::AnalysisDiagram::FALLBACK_NOTICE,
            ));
            if let Some(reason) = &diagram.fallback_reason {
                body = body.push(notice(reason));
            }
        }
        if let Some(handle) = state.diagram_preview {
            body = body.push(
                button(svg(handle.clone()).width(Length::Fill).height(220))
                    .style(button::secondary)
                    .width(Length::Fill)
                    .on_press(wrap(Message::Diagram)),
            );
        } else {
            body = body.push(notice("The stored flowchart could not be rendered."));
        }
    } else {
        body = body.push(notice("No flowchart was stored for this analysis."));
    }
    body
}

fn report<'a>(
    record: &'a Record,
    project: bool,
    can_jump: bool,
    selected: Option<&crate::snippet_analysis::model::Selection>,
) -> Column<'a, WindowMessage> {
    let analysis = &record.analysis;
    let provenance = &record.provenance;
    let mut body = column![
        text("Report").size(17),
        text(history_choice(record).label).size(13),
        (!project).then(|| {
            text(format!(
                "{} · {} lines",
                record.source.language, record.source.line_count
            ))
            .size(13)
        }),
        (!provenance.model.is_empty())
            .then(|| text(format!("Model · {}", provenance.model)).size(13)),
        (!provenance.skill_names.is_empty()).then(|| text(format!(
            "Skills · {}",
            provenance.skill_names.join(", ")
        ))
        .size(13)),
        (!analysis.summary.is_empty())
            .then(|| text(&analysis.summary).wrapping(Wrapping::WordOrGlyph)),
    ]
    .spacing(8);
    if !provenance.knowledge_citations.is_empty() {
        body = body.push(text("Knowledge citations").size(16));
        for (index, citation) in provenance.knowledge_citations.iter().enumerate() {
            body = body.push(
                text(format!(
                    "[R{}] · {} / {} · {}",
                    index + 1,
                    citation.store,
                    citation.file,
                    citation.location()
                ))
                .size(13)
                .wrapping(Wrapping::WordOrGlyph),
            );
        }
    }
    if project && !analysis.project_files.is_empty() {
        body = body.push(text("Project files").size(16));
        for file in &analysis.project_files {
            body = body.push(
                column![
                    text(&file.path).size(13).wrapping(Wrapping::WordOrGlyph),
                    (!file.purpose.is_empty())
                        .then(|| text(&file.purpose).size(13).wrapping(Wrapping::WordOrGlyph)),
                ]
                .spacing(3),
            );
        }
    }
    for (category, label) in [
        ("security", "Security"),
        ("optimization", "Optimization"),
        ("design", "Design"),
    ] {
        let mut findings: Vec<_> = analysis
            .improvements
            .iter()
            .filter(|finding| in_category(finding, category))
            .collect();
        if !findings.is_empty() {
            findings.sort_by_key(|finding| severity_rank(&finding.severity));
            body = body.push(text(format!("{label} ({})", findings.len())).size(16));
            for finding in findings {
                let badge = verification_badge(record, &finding.id);
                body = body.push(finding_card(
                    finding,
                    project,
                    can_jump,
                    badge,
                    selected.map(|selection| (record.id, selection.findings.contains(&finding.id))),
                ));
            }
        }
    }
    if analysis.improvements.is_empty() {
        body = body.push(text("No improvements found.").size(13));
    }
    if !analysis.dependencies.is_empty() {
        body = body.push(text(format!("Dependencies ({})", analysis.dependencies.len())).size(16));
        for dependency in &analysis.dependencies {
            body = body.push(dependency_card(
                dependency,
                verification_badge(record, &dependency.id),
                selected
                    .filter(|_| !dependency.suggestion.trim().is_empty())
                    .map(|selection| (record.id, selection.dependencies.contains(&dependency.id))),
            ));
        }
    }
    body
}

fn verification_badge(record: &Record, id: &str) -> Option<String> {
    let verification = record.verification.as_ref()?;
    verification
        .persisting_current_to_previous
        .get(id)
        .map(|previous| format!("Still open (was {previous})"))
        .or_else(|| {
            verification
                .new_ids
                .iter()
                .any(|item| item == id)
                .then(|| "New".into())
        })
}

fn in_category(finding: &Finding, category: &str) -> bool {
    if category == "design" {
        !finding.category.eq_ignore_ascii_case("security")
            && !finding.category.eq_ignore_ascii_case("optimization")
    } else {
        finding.category.eq_ignore_ascii_case(category)
    }
}

fn severity_rank(severity: &str) -> usize {
    ["critical", "high", "medium", "low", "info"]
        .iter()
        .position(|rank| severity.eq_ignore_ascii_case(rank))
        .unwrap_or(5)
}

fn finding_card(
    finding: &Finding,
    project: bool,
    can_jump: bool,
    verification_badge: Option<String>,
    apply_selection: Option<(SnippetId, bool)>,
) -> Element<'_, WindowMessage> {
    let rank = severity_rank(&finding.severity);
    let mut title: Row<'_, WindowMessage> = row![
        apply_selection.map(|(record, selected)| checkbox(selected)
            .label("Select improvement")
            .on_toggle(move |selected| apply::msg(apply::Action::Finding(
                record,
                finding.id.clone(),
                selected
            )))),
        text(&finding.severity)
            .size(12)
            .style(move |theme: &Theme| text::Style {
                color: (rank < 2).then(|| theme.extended_palette().danger.strong.color),
            }),
        text(&finding.id).size(12),
        text(&finding.title)
            .size(14)
            .wrapping(Wrapping::WordOrGlyph),
        verification_badge.map(|label| text(label).size(12)),
    ];
    if let Some(line) = finding.line.filter(|line| *line > 0) {
        title = title.push(
            button(text(format!("Line {line}")).size(12))
                .padding([2, 4])
                .style(button::text)
                .on_press_maybe((can_jump && (!project || finding.file.is_some())).then(|| {
                    wrap(Message::Jump {
                        file: finding.file.clone(),
                        line,
                    })
                })),
        );
    }
    card(
        column![
            title.spacing(6).align_y(Alignment::Center).wrap(),
            project.then(|| {
                text(finding.file.as_deref().unwrap_or("Cross-file"))
                    .size(12)
                    .wrapping(Wrapping::WordOrGlyph)
            }),
            (!finding.detail.is_empty())
                .then(|| text(&finding.detail).wrapping(Wrapping::WordOrGlyph)),
            (!finding.recommendation.is_empty()).then(|| {
                column![
                    text("Recommendation").size(12),
                    text(&finding.recommendation).wrapping(Wrapping::WordOrGlyph),
                ]
                .spacing(3)
            }),
        ]
        .spacing(5),
    )
}

fn dependency_card(
    dependency: &Dependency,
    verification_badge: Option<String>,
    apply_selection: Option<(SnippetId, bool)>,
) -> Element<'_, WindowMessage> {
    card(
        column![
            row![
                apply_selection.map(|(record, selected)| checkbox(selected)
                    .label("Select dependency change")
                    .on_toggle(move |selected| apply::msg(apply::Action::Dependency(
                        record,
                        dependency.id.clone(),
                        selected
                    )))),
                text(&dependency.id).size(12),
                text(&dependency.kind).size(12),
                text(&dependency.name).wrapping(Wrapping::WordOrGlyph),
                verification_badge.map(|label| text(label).size(12)),
            ]
            .spacing(6)
            .wrap(),
            (!dependency.purpose.is_empty())
                .then(|| text(&dependency.purpose).wrapping(Wrapping::WordOrGlyph)),
            (!dependency.suggestion.is_empty()).then(|| {
                column![
                    text("Reduce or replace").size(12),
                    text(&dependency.suggestion).wrapping(Wrapping::WordOrGlyph),
                ]
                .spacing(3)
            }),
        ]
        .spacing(5),
    )
}

fn history_choice(record: &Record) -> HistoryChoice {
    let when = i64::try_from(record.analyzed_at)
        .ok()
        .and_then(|millis| jiff::Timestamp::from_millisecond(millis).ok())
        .map(|time| {
            time.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "Unknown time".into());
    let provenance = &record.provenance;
    let kind = match record.purpose {
        Purpose::Analysis => "Analysis",
        Purpose::Rerun => "Rerun",
        Purpose::Verify => "Verification",
    };
    let profile = if provenance.profile_name.is_empty() {
        "Default profile"
    } else {
        provenance.profile_name.as_str()
    };
    let pin = if record.pinned { "Pinned · " } else { "" };
    let mut label = format!("{pin}{when} · {profile} · {kind}");
    if provenance.usage.total_tokens > 0 {
        label.push_str(&format!(" · {} tokens", provenance.usage.total_tokens));
    }
    if provenance.duration_millis > 0 {
        label.push_str(&format!(
            " · {:.1}s",
            provenance.duration_millis as f64 / 1000.0
        ));
    }
    HistoryChoice {
        id: record.id,
        label,
    }
}

fn card(contents: Column<'_, WindowMessage>) -> Element<'_, WindowMessage> {
    container(contents)
        .style(container::bordered_box)
        .padding(8)
        .width(Length::Fill)
        .into()
}

fn notice(message: &str) -> Element<'_, WindowMessage> {
    card(column![
        text(message).size(13).wrapping(Wrapping::WordOrGlyph)
    ])
}

fn action(label: &'static str, message: Message) -> iced::widget::Button<'static, WindowMessage> {
    button(label).on_press(wrap(message))
}

fn wrap(message: Message) -> WindowMessage {
    WindowMessage::Analysis(message)
}
