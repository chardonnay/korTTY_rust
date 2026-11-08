//! Native review surfaces for snippet and folder analyses.

use std::fmt;

use iced::advanced::text::Wrapping;
use iced::widget::{
    Column, Row, button, checkbox, column, container, keyed_column, pick_list, row, rule,
    scrollable, text, text_input,
};
use iced::{Alignment, Element, Font, Length, Theme};

use crate::snippet_analysis::model::{Dependency, Finding, History, Purpose, Record, Subject};
use crate::snippets::SnippetId;

use super::Message as WindowMessage;

#[derive(Clone)]
pub enum Message {
    Tab(Subject),
    Editor,
    Close,
    Profile(u64),
    Instructions(String),
    Files(bool),
    CancelFiles,
    IncludeFile(SnippetId, bool),
    RequestStart,
    ConfirmStart,
    CancelStart,
    ReviewPage(bool),
    Abort,
    SelectRecord(SnippetId),
    Pin(bool),
    RequestDiscard,
    Discard,
    CancelDiscard,
    Compare,
    Diagram,
    Export,
    Reports(crate::workspace::SnippetReportAction),
    Jump { file: Option<String>, line: usize },
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct ProfileChoice {
    pub(crate) id: u64,
    pub(crate) name: String,
    pub(crate) model: String,
    pub(crate) endpoint: String,
}

impl fmt::Display for ProfileChoice {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(&self.name)
    }
}

#[derive(Clone)]
pub(crate) struct FileChoice {
    pub(crate) id: SnippetId,
    pub(crate) path: String,
    pub(crate) bytes: usize,
    pub(crate) included: bool,
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
    },
    Running {
        status: &'a str,
    },
    ConfirmDiscard,
}

pub(crate) struct View<'a> {
    pub(crate) subject: Subject,
    pub(crate) name: &'a str,
    pub(crate) profiles: &'a [ProfileChoice],
    pub(crate) profile: Option<u64>,
    pub(crate) instructions: &'a str,
    pub(crate) files: &'a [FileChoice],
    pub(crate) estimated_tokens: usize,
    pub(crate) history: Option<&'a History>,
    pub(crate) record: Option<SnippetId>,
    pub(crate) phase: Phase<'a>,
    pub(crate) stale: bool,
    pub(crate) status: Option<&'a str>,
    pub(crate) can_export: bool,
    pub(crate) can_compare: bool,
    pub(crate) can_jump: bool,
    pub(crate) can_diagram: bool,
    pub(crate) scroll: iced::widget::Id,
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
    let running = matches!(state.phase, Phase::Running { .. });
    let title = if project {
        "Project analysis"
    } else {
        "Code analysis"
    };

    let header = row![
        text(title).size(20),
        action("Editor", Message::Editor),
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

    let (body, actions, phase_key) = match state.phase {
        Phase::Files => (
            file_chooser(state.files),
            row![
                action("Cancel", Message::CancelFiles),
                action("Done", Message::Files(false)),
            ],
            1,
        ),
        Phase::ConfirmStart { preview, .. } => (
            confirmation(project, profile, included, preview),
            row![
                action("Back to report", Message::CancelStart),
                button("Start analysis")
                    .on_press_maybe(can_start.then_some(wrap(Message::ConfirmStart))),
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
        Phase::Ready | Phase::Running { .. } => {
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
            } else {
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
        _ => (0, None),
    };
    let key = (phase_key, record.map(|record| record.id), preview_page);
    let progress = match state.phase {
        Phase::Running { status } => Some(notice(status)),
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
    if matches!(state.phase, Phase::Ready) {
        body = body.push(profile_picker(state)).push(
            text_input("Additional instructions (optional)", state.instructions)
                .on_input(|instructions| wrap(Message::Instructions(instructions))),
        );
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
        state
            .can_diagram
            .then(|| action("Flow diagram", Message::Diagram)),
        button("Export report…").on_press_maybe(state.can_export.then_some(wrap(Message::Export))),
        button("Compare reports")
            .on_press_maybe(state.can_compare.then_some(wrap(Message::Compare))),
    ];
    if matches!(state.phase, Phase::Ready) {
        tools = tools
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
    body.push(report(record, project, state.can_jump))
}

fn profile_picker<'a>(state: &View<'a>) -> Element<'a, WindowMessage> {
    if state.profiles.is_empty() {
        return notice("Configure a coding profile in the AI Manager to start an analysis.");
    }
    let selected = state
        .profiles
        .iter()
        .find(|profile| Some(profile.id) == state.profile);
    row![
        text("AI profile").size(13),
        pick_list(state.profiles, selected, |profile| wrap(Message::Profile(
            profile.id
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
    profile: Option<&'a ProfileChoice>,
    included: usize,
    preview: &'a str,
) -> Column<'a, WindowMessage> {
    let mut body = column![
        text("New analysis").size(17),
        text(if project {
            format!("Send the source of {included} selected files to this AI profile?")
        } else {
            "Send the current snippet source to this AI profile?".into()
        }),
    ]
    .spacing(8);
    if let Some(profile) = profile {
        body = body
            .push(text(format!("AI profile · {}", profile.name)).wrapping(Wrapping::WordOrGlyph));
        if !profile.model.is_empty() {
            body = body
                .push(text(format!("Model · {}", profile.model)).wrapping(Wrapping::WordOrGlyph));
        }
        if !profile.endpoint.is_empty() {
            body = body.push(
                text(format!("Endpoint · {}", profile.endpoint)).wrapping(Wrapping::WordOrGlyph),
            );
        }
    }
    body.push(text("Request preview").size(16)).push(
        text(preview)
            .font(Font::MONOSPACE)
            .size(12)
            .wrapping(Wrapping::WordOrGlyph),
    )
}

fn report(record: &Record, project: bool, can_jump: bool) -> Column<'_, WindowMessage> {
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
                body = body.push(finding_card(finding, project, can_jump));
            }
        }
    }
    if analysis.improvements.is_empty() {
        body = body.push(text("No improvements found.").size(13));
    }
    if !analysis.dependencies.is_empty() {
        body = body.push(text(format!("Dependencies ({})", analysis.dependencies.len())).size(16));
        for dependency in &analysis.dependencies {
            body = body.push(dependency_card(dependency));
        }
    }
    body
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

fn finding_card(finding: &Finding, project: bool, can_jump: bool) -> Element<'_, WindowMessage> {
    let rank = severity_rank(&finding.severity);
    let mut title: Row<'_, WindowMessage> = row![
        text(&finding.severity)
            .size(12)
            .style(move |theme: &Theme| text::Style {
                color: (rank < 2).then(|| theme.extended_palette().danger.strong.color),
            }),
        text(&finding.id).size(12),
        text(&finding.title)
            .size(14)
            .wrapping(Wrapping::WordOrGlyph),
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

fn dependency_card(dependency: &Dependency) -> Element<'_, WindowMessage> {
    card(
        column![
            row![
                text(&dependency.id).size(12),
                text(&dependency.kind).size(12),
                text(&dependency.name).wrapping(Wrapping::WordOrGlyph),
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
