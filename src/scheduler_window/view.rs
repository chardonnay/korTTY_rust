use std::borrow::Cow;
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use iced::widget::{
    button, checkbox, column, container, pane_grid, pick_list, row, rule, scrollable, svg, text,
    text_input,
};
use iced::{Alignment, Element, Length};
use kortty_diagram::{Direction, Scene, Theme};
use kortty_jobs::{
    BlockReason, Blocker, DiagramScope, JobId, RunId, RunOutcome, RunRecord, RunTrigger, Scheduler,
};

use super::journal_search::{JournalColumn, JournalSearchMode};
use super::{
    Confirmation, MAX_JOURNAL_RETENTION_DAYS, Message, Page, RemoteAction, RunProposalView,
    SchedulerView, SchedulerWindow, Scope, SshCommand, sync,
};
use crate::diagram_window::Format;

mod journal_details;
mod run_proposal;
mod swarm_report;

pub(in crate::scheduler_window) fn reviewed_cli_payload<'a>(
    request: &crate::local_ai::cli::Request,
) -> Element<'a, Message> {
    let (label, payload) = request.primary();
    let payload_text = |value: String| {
        text(value)
            .font(iced::Font::MONOSPACE)
            .wrapping(text::Wrapping::WordOrGlyph)
    };
    column![
        text(label).size(13),
        payload_text(payload.to_owned()),
        request.secondary().map(|input| column![
            text("Exact UTF-8 user-prompt.txt").size(13),
            payload_text(input.to_owned())
        ]
        .spacing(4)),
    ]
    .spacing(4)
    .into()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JournalSortColumn {
    Started,
    Status,
    Job,
    SessionJournal,
    Summary,
}

impl JournalSortColumn {
    const ALL: [Self; 5] = [
        Self::Started,
        Self::Status,
        Self::Job,
        Self::SessionJournal,
        Self::Summary,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Started => "Started",
            Self::Status => "Status",
            Self::Job => "Job",
            Self::SessionJournal => "Session journal",
            Self::Summary => "Summary",
        }
    }
}

pub(super) struct JournalSort {
    column: JournalSortColumn,
    descending: bool,
}

impl Default for JournalSort {
    fn default() -> Self {
        Self {
            column: JournalSortColumn::Started,
            descending: true,
        }
    }
}

impl JournalSort {
    pub(super) fn select(&mut self, column: JournalSortColumn) {
        self.descending = self.column == column && !self.descending;
        self.column = column;
    }

    fn header(&self, column: JournalSortColumn) -> Element<'static, Message> {
        let direction = if self.column != column {
            ""
        } else if self.descending {
            " ↓"
        } else {
            " ↑"
        };
        button(text(format!("{}{direction}", column.label())).size(12))
            .style(button::subtle)
            .padding([2, 0])
            .on_press(Message::JournalSort(column))
            .into()
    }

    fn order(&self, rows: &mut [JournalRow<'_>]) {
        rows.sort_by(|left, right| {
            let primary = match self.column {
                JournalSortColumn::Started => left.started_at.cmp(&right.started_at),
                JournalSortColumn::Status => compare_label(left.status, right.status),
                JournalSortColumn::Job => compare_label(left.name, right.name),
                JournalSortColumn::SessionJournal => {
                    left.session_journals.cmp(&right.session_journals)
                }
                JournalSortColumn::Summary => compare_label(&left.summary, &right.summary),
            };
            let primary = if self.descending {
                primary.reverse()
            } else {
                primary
            };
            primary.then_with(|| right.started_at.cmp(&left.started_at))
        });
    }
}

fn compare_label(left: &str, right: &str) -> std::cmp::Ordering {
    left.chars()
        .flat_map(char::to_lowercase)
        .cmp(right.chars().flat_map(char::to_lowercase))
}

pub(super) struct JournalRow<'a> {
    pub(super) id: RunId,
    started_at: SystemTime,
    started: String,
    status: &'static str,
    name: &'a str,
    session_journals: usize,
    summary: Cow<'a, str>,
    active: Option<Vec<RunProposalView<'a>>>,
}

fn agent_review(review: &super::agent::Review) -> Element<'_, Message> {
    use super::agent::{
        ModelState, checked_proposal, invocation,
        proposal::{MAX_SWARM_COMMAND_ROUNDS, MAX_SWARM_STREAM_BYTES},
        skill_review::Classification,
    };

    let profile = &review.approval.profile;
    if let Some(comparison) = &review.comparison_review {
        return comparison.view(profile);
    }
    let initial_probe = review
        .model
        .as_ref()
        .and_then(|model| model.prompts.first())
        .and_then(|prompt| prompt.swarm_probe_command());
    let mut details = column![
        text(if review.swarm_iterative() {
            "Review scheduled iterative AI Swarm requests"
        } else if review.per_firing() {
            "Review scheduled AI Agent proposal requests"
        } else if review.advisory() {
            "Review read-only AI diagnostic requests"
        } else if review.model.is_some() {
            "Review one-use SSH workflow"
        } else {
            "Review scheduled diagnostics"
        })
        .size(17),
        text(format!("AI profile · {profile}")),
        text(format!("AI destination · {}", profile.destination)).size(12),
        text(&review.command).font(iced::Font::MONOSPACE),
    ]
    .spacing(5);
    if let Some(command) = review
        .model
        .as_ref()
        .and_then(|model| model.prompts.first())
        .and_then(|prompt| prompt.transport.cli_command_review())
    {
        details = details.push(
            text(command)
                .size(12)
                .font(iced::Font::MONOSPACE)
                .wrapping(text::Wrapping::WordOrGlyph),
        );
    }
    if let Some(parallelism) = review.swarm_parallelism {
        details = details.push(text(format!("Target parallelism limit · {parallelism}")));
    }
    if review.swarm_iterative() {
        details = if let Some(probe) = initial_probe {
            details
                .push(text("This approval allows the fixed SSH probe to run once on each target before its first model request. The model receives the masked probe result.").size(13))
                .push(
                    text(probe)
                        .font(iced::Font::MONOSPACE)
                        .wrapping(text::Wrapping::WordOrGlyph),
                )
                .push(text(format!(
                    "The probe contributes up to {MAX_SWARM_STREAM_BYTES} bytes each of masked standard output and standard error for its own target."
                )).size(13))
        } else {
            details.push(text("Each target starts with empty SSH evidence.").size(13))
        };
        details = details
            .push(text("Approve the exact commands for every model round in the Journal before they run.").size(13))
            .push(text(format!(
                "Each target can use up to {MAX_SWARM_COMMAND_ROUNDS} command rounds and one final request for a conclusion. Follow-up requests use that target's command results, including up to {MAX_SWARM_STREAM_BYTES} bytes each of masked standard output and standard error per command."
            )).size(13));
    }
    for target in &review.approval.targets {
        details = details.push(text(format!(
            "Target {} · {}",
            target.reference.id,
            target.reference.target.label()
        )));
        if let Some(jump) = &target.reference.jump {
            details = details.push(text(format!("Via {}", jump.label())).size(12));
        }
        for pin in &target.pins {
            details = details.push(text(pin).size(12));
        }
    }
    let mut actions =
        row![button("Discard review").on_press(Message::DiscardAgentReview)].spacing(6);
    let approve_model_requests = |preview_ready: bool| {
        button(if initial_probe.is_some() || review.advisory() {
            "Approve probe and model requests"
        } else {
            "Approve model requests"
        })
        .on_press_maybe(
            review
                .firing_review_ticket()
                .filter(|_| preview_ready)
                .map(str::to_owned)
                .map(Message::ApproveFiringRequest),
        )
    };
    let model = review.model.as_ref();
    let skills = model.and_then(|model| model.skills.as_ref());
    let classification = skills.map(|skills| &skills.classification);
    if let (Some(model), Some(skills), Some(Classification::Closed)) =
        (model, skills, classification)
        && matches!(model.state, ModelState::Preview)
        && let Some(knowledge) = &model.knowledge
    {
        if review.standing() {
            details = details.push(text("Approved excerpts are saved with this job. Changes to the index, its sources, or a linked Qdrant vault require a new review.").size(13));
        }
        details = details.push(
            knowledge
                .view(true)
                .map(|message| Message::ProposalKnowledge(skills.preview.clone(), message)),
        );
    }
    if let (Some(ModelState::Preview), Some(skills), Some(Classification::Closed)) =
        (model.map(|model| &model.state), skills, classification)
    {
        if let Some(forecast) = skills.quota.label() {
            if review.standing() {
                details = details.push(text(if review.advisory() {
                    "Quota forecast for all read-only target requests per firing. Actual input includes masked probe output."
                } else if review.swarm_iterative() {
                    "Quota forecast for all starting target requests per firing. Probe results and later rounds affect actual usage."
                } else {
                    "Quota forecast for all target requests per firing."
                }).size(13));
            }
            details = details.push(text(forecast).size(13));
        }
        if skills.has_classifier() && review.knowledge_ready() {
            actions = actions.push(
                button("Review skill classification…")
                    .on_press(Message::ReviewProposalSkills(skills.preview.clone())),
            );
        }
    }
    if review.supports_comparison()
        && let Some(ticket) = review.firing_review_ticket()
    {
        if review.approval.comparison.is_some() {
            details =
                details.push(text("AI comparison approved for each completed firing.").size(13));
            actions = actions.push(
                button("Remove comparison approval")
                    .on_press(Message::CancelSwarmComparison(ticket.to_owned())),
            );
        } else {
            actions = actions.push(
                button("Review scheduled comparison…")
                    .on_press(Message::ReviewSwarmComparison(ticket.to_owned())),
            );
        }
    }
    match (
        model.map(|model| (&model.state, model.prompts.as_slice())),
        skills,
        classification,
    ) {
        (
            Some((ModelState::Preview, _)),
            Some(skills),
            Some(Classification::Preview {
                text: preview,
                quota,
            }),
        ) => {
            details = details.push(text("Review masked skill classification request").size(13));
            if let Some(forecast) = quota.label() {
                details = details.push(text(forecast).size(13));
            }
            details = details.push(
                text(preview.as_str())
                    .font(iced::Font::MONOSPACE)
                    .wrapping(text::Wrapping::WordOrGlyph),
            );
            actions = actions
                .push(
                    button("Back to model review")
                        .on_press(Message::CancelProposalSkills(skills.preview.clone())),
                )
                .push(
                    button("Classify skills").on_press_maybe(
                        (!quota.loading())
                            .then(|| Message::ClassifyProposalSkills(skills.preview.clone())),
                    ),
                );
        }
        (Some((ModelState::Preview, _)), Some(skills), Some(Classification::Pending { .. })) => {
            details = details.push(text("Waiting for the reviewed skill classification…"));
            actions = actions.push(
                button("Cancel classification")
                    .on_press(Message::CancelProposalSkills(skills.preview.clone())),
            );
        }
        (Some((ModelState::Preview, [prompt])), Some(skills), _) if !review.standing() => {
            let preview_ready = match prompt.cli_review_input(false) {
                Ok(Some(request)) => {
                    details = details.push(reviewed_cli_payload(&request));
                    true
                }
                Ok(None) => {
                    let [system, input] = prompt.messages();
                    details = details
                        .push(text("System instructions").size(13))
                        .push(text(system).size(12).wrapping(text::Wrapping::WordOrGlyph))
                        .push(text("Masked request").size(13))
                        .push(
                            text(input)
                                .font(iced::Font::MONOSPACE)
                                .wrapping(text::Wrapping::WordOrGlyph),
                        );
                    true
                }
                Err(error) => {
                    details = details.push(text(error).size(13));
                    false
                }
            };
            actions = actions.push(
                button("Ask model for proposal").on_press_maybe(
                    (skills.ready() && review.knowledge_ready() && preview_ready)
                        .then(|| Message::RequestAgentProposal(skills.preview.clone())),
                ),
            );
        }
        (Some((ModelState::Preview, prompts)), _, _) => {
            let show_target_instructions = prompts
                .iter()
                .any(|prompt| prompt.firing_skill_guidance().is_some());
            let cli = prompts
                .first()
                .is_some_and(|prompt| prompt.transport.is_cli());
            details = details.push(
                text(if review.advisory() {
                    "Each target runs the fixed SSH probe before its read-only model request. Only masked output from that target fills the placeholder."
                } else if review.swarm_iterative() {
                    "These model instructions and masked inputs form the starting requests."
                } else {
                    "The model receives the following instructions and masked input."
                })
                .size(13),
            );
            if !cli
                && !show_target_instructions
                && let Some(prompt) = prompts.first()
            {
                details = details.push(text(prompt.firing_system()).size(12));
            }
            let mut preview_ready = true;
            for (target, prompt) in review.approval.targets.iter().zip(prompts) {
                if cli {
                    details = details.push(text(format!(
                        "CLI input for target {} · {}",
                        target.reference.id,
                        target.reference.target.label()
                    )));
                    match prompt.cli_review_input(true) {
                        Ok(Some(request)) => details = details.push(reviewed_cli_payload(&request)),
                        Ok(None) => preview_ready = false,
                        Err(error) => {
                            preview_ready = false;
                            details = details.push(text(error).size(13));
                        }
                    }
                    continue;
                }
                let input = if show_target_instructions {
                    let instructions = match prompt.firing_skill_guidance() {
                        Some("") => "System instructions · no skill guidance",
                        Some(_) => "System instructions and reviewed skill guidance",
                        None => "System instructions",
                    };
                    details = details
                        .push(text(format!(
                            "Model request for target {} · {}",
                            target.reference.id,
                            target.reference.target.label()
                        )))
                        .push(text(instructions).size(13))
                        .push(
                            text(prompt.firing_system())
                                .size(12)
                                .wrapping(text::Wrapping::WordOrGlyph),
                        )
                        .push(
                            text(
                                if prompt.is_advisory() || prompt.swarm_probe_command().is_some() {
                                    "Initial masked request template (placeholder probe result)"
                                } else if review.swarm_iterative() {
                                    "Initial masked request"
                                } else {
                                    "Masked request"
                                },
                            )
                            .size(13),
                        );
                    prompt.firing_input_preview().map(text)
                } else {
                    details = details.push(text(format!(
                        "Masked request for target {} · {}",
                        target.reference.id,
                        target.reference.target.label()
                    )));
                    Ok(text(prompt.input.as_str()))
                };
                match input {
                    Ok(input) => {
                        details = details.push(
                            input
                                .font(iced::Font::MONOSPACE)
                                .wrapping(text::Wrapping::WordOrGlyph),
                        )
                    }
                    Err(error) => {
                        preview_ready = false;
                        details = details.push(text(error).size(13));
                    }
                }
            }
            if review.per_firing() && !review.swarm_iterative() {
                details = details.push(text("Each firing asks the model for a new proposal per target. Review the exact commands in the Journal before allowing them to run.").size(13));
            }
            actions = actions.push(approve_model_requests(preview_ready));
        }
        (Some((ModelState::Pending { .. }, _)), _, _) => {
            details = details.push(text("Waiting for the reviewed model proposal…"));
        }
        (Some((ModelState::Ready { items, ticket }, _)), _, _) => {
            details = details.push(text("These commands can change the remote systems under the listed SSH accounts. Approval is consumed before the first command starts.").size(13));
            for item in items.iter() {
                let checked = checked_proposal(&item.command, &review.directory);
                details = details
                    .push(text(format!("Target {} · {}", item.target, item.purpose)))
                    .push(text(&item.command).font(iced::Font::MONOSPACE))
                    .push(
                        text(invocation(&review.directory, &checked))
                            .font(iced::Font::MONOSPACE)
                            .size(12),
                    );
            }
            actions = actions.push(
                button("Approve one run")
                    .on_press(Message::ApproveProposalCommands(ticket.clone())),
            );
        }
        (None, _, _) => {
            if let Some(question) = &review.advisory_question {
                details = details
                    .push(text("Each model request contains this masked question and the fixed probe output for one target.").size(13))
                    .push(text(question).font(iced::Font::MONOSPACE).wrapping(text::Wrapping::WordOrGlyph));
                actions = actions.push(approve_model_requests(true));
            } else {
                actions =
                    actions.push(button("Approve diagnostics").on_press(Message::ApproveAgent));
            }
        }
    }
    column![
        scrollable(details).height(Length::Fixed(190.0)),
        actions.wrap(),
    ]
    .spacing(6)
    .into()
}

pub(super) struct Layout {
    pub(super) jobs: pane_grid::State<bool>,
    pub(super) journal: pane_grid::State<bool>,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            jobs: split(pane_grid::Axis::Vertical, 0.27),
            journal: split(pane_grid::Axis::Horizontal, 0.72),
        }
    }
}

fn split(axis: pane_grid::Axis, ratio: f32) -> pane_grid::State<bool> {
    let (mut state, first) = pane_grid::State::new(true);
    if let Some((_, divider)) = state.split(axis, first, false) {
        state.resize(divider, ratio);
    }
    state
}

pub(super) struct DiagramView {
    pub scope: Scope,
    direction: Direction,
    pub(super) theme: Theme,
    scale: f32,
    pub(super) preview: Option<DiagramPreview>,
    pub(super) path: String,
    error: Option<String>,
}

pub(super) struct DiagramPreview {
    pub(super) scene: Arc<Scene>,
    pub(super) mermaid: String,
    handle: svg::Handle,
}

impl Default for DiagramView {
    fn default() -> Self {
        Self {
            scope: Scope::All,
            direction: Direction::TopToBottom,
            theme: Theme::Dark,
            scale: 1.0,
            preview: None,
            path: String::new(),
            error: None,
        }
    }
}

impl DiagramView {
    pub fn refresh(&mut self, scheduler: &Scheduler<SshCommand>, selected: Option<&JobId>) {
        let scope = match self.scope {
            Scope::All => DiagramScope::All,
            Scope::Connected => DiagramScope::Connected,
            Scope::Selected => selected.map_or(DiagramScope::Connected, DiagramScope::Neighborhood),
        };
        self.preview = None;
        self.error = None;
        match scheduler.diagram(self.direction, scope) {
            Ok(diagram) if !diagram.nodes().is_empty() => {
                let scene = Arc::new(diagram.layout());
                self.preview = Some(DiagramPreview {
                    handle: svg::Handle::from_memory(scene.to_svg(self.theme).into_bytes()),
                    scene,
                    mermaid: diagram.to_styled_mermaid(self.theme),
                });
            }
            Ok(_) => {}
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    pub fn zoom(&mut self, factor: f32) {
        self.scale = if factor == 0.0 {
            1.0
        } else {
            (self.scale * factor).clamp(0.25, 2.0)
        };
    }

    pub fn toggle_direction(&mut self) {
        self.direction = match self.direction {
            Direction::TopToBottom => Direction::LeftToRight,
            Direction::LeftToRight => Direction::TopToBottom,
        };
    }

    pub fn toggle_theme(&mut self) {
        self.theme = match self.theme {
            Theme::Light => Theme::Dark,
            Theme::Dark => Theme::Light,
        };
    }

    fn view(&self) -> Element<'_, Message> {
        let mut controls = row![].spacing(6).align_y(Alignment::Center);
        for (scope, label) in [
            (Scope::All, "All jobs"),
            (Scope::Connected, "Connected"),
            (Scope::Selected, "Selected job"),
        ] {
            controls = controls.push(
                button(label)
                    .style(tab_style(self.scope == scope))
                    .on_press(Message::DiagramScope(scope)),
            );
        }
        controls = controls.extend([
            button(match self.direction {
                Direction::TopToBottom => "Vertical",
                Direction::LeftToRight => "Horizontal",
            })
            .on_press(Message::DiagramDirection)
            .into(),
            button(match self.theme {
                Theme::Dark => "Dark",
                Theme::Light => "Light",
            })
            .on_press(Message::DiagramTheme)
            .into(),
            button("−").on_press(Message::Zoom(0.8)).into(),
            button(text(format!("{:.0}%", self.scale * 100.0)))
                .on_press(Message::Zoom(0.0))
                .into(),
            button("+").on_press(Message::Zoom(1.25)).into(),
            button("Copy Mermaid")
                .on_press_maybe(self.preview.is_some().then_some(Message::DiagramCopy))
                .into(),
        ]);
        let contents: Element<'_, Message> = if let Some(preview) = &self.preview {
            let bounds = preview.scene.bounds();
            let width = bounds.width as f32;
            let height = bounds.height as f32;
            let scale = self.scale.min(4096.0 / width.max(height));
            scrollable(
                svg(preview.handle.clone())
                    .width(width * scale)
                    .height(height * scale),
            )
            .direction(scrollable::Direction::Both {
                vertical: scrollable::Scrollbar::new(),
                horizontal: scrollable::Scrollbar::new(),
            })
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
        } else {
            text(self.error.as_deref().unwrap_or("No jobs in this diagram")).into()
        };
        let mut panel = column![
            controls,
            text("Solid arrows run a chain. Dashed arrows are conditions.").size(13),
            contents
        ]
        .spacing(8);
        if self.preview.is_some() {
            let valid = !self.path.trim().is_empty();
            panel = panel.push(
                row![
                    text_input("Absolute export path", &self.path).on_input(Message::DiagramPath),
                    button("Save SVG")
                        .on_press_maybe(valid.then_some(Message::DiagramExport(Format::Svg))),
                    button("Save PNG")
                        .on_press_maybe(valid.then_some(Message::DiagramExport(Format::Png))),
                ]
                .spacing(6),
            );
        }
        panel.height(Length::Fill).into()
    }
}

impl SchedulerView {
    /// Renders the scheduler as native Iced controls.
    pub(crate) fn view<'a>(
        &'a self,
        runtime: &'a SchedulerWindow,
        menu_status_visible: bool,
        privileged_allowed: bool,
        session_journal_root: Option<&Path>,
    ) -> Element<'a, Message> {
        if let Some(confirm @ Confirmation::Close) = &self.confirm {
            return container(self.confirm_view(runtime, confirm))
                .padding(12)
                .center(Length::Fill)
                .into();
        }
        if let Some(browser) = &self.directory_browser {
            return browser.view();
        }
        let contents = pane_grid(&self.layout.jobs, |_, first, _| {
            pane_grid::Content::new(if *first {
                self.jobs_view(runtime, menu_status_visible, session_journal_root)
            } else {
                self.details_view(runtime, privileged_allowed, session_journal_root)
            })
        })
        .spacing(8)
        .min_size(168)
        .on_resize(6, Message::ResizeJobs);
        column![
            text("Scheduled background automation").size(20),
            contents,
            rule::horizontal(1),
            row![
                iced::widget::space::horizontal(),
                button("Close")
                    .style(button::secondary)
                    .on_press(Message::Close),
            ],
        ]
        .spacing(10)
        .padding(12)
        .height(Length::Fill)
        .into()
    }

    fn jobs_view<'a>(
        &'a self,
        runtime: &'a SchedulerWindow,
        menu_status_visible: bool,
        session_journal_root: Option<&Path>,
    ) -> Element<'a, Message> {
        let graph = runtime.scheduler.graph();
        let journal_stats =
            session_journal_root.and_then(|root| runtime.journal_stats.result(root));
        let mut entries = column![].spacing(1);
        for job in graph.jobs() {
            let selected = self.draft.id.as_ref() == Some(job.id());
            let next = if runtime.scheduler.is_running(job.id()) {
                "Running".to_owned()
            } else {
                runtime
                    .scheduler
                    .scheduled_at(job.id())
                    .map_or_else(String::new, |time| runtime.local_time(time))
            };
            let dependencies = job
                .dependencies()
                .iter()
                .map(|id| graph.job(id).map_or(id.as_str(), |job| job.name()))
                .collect::<Vec<_>>()
                .join(", ");
            let journal_tokens = journal_stats
                .and_then(|result| result.as_ref().ok())
                .map_or_else(String::new, |report| {
                    report.cell(
                        job.id(),
                        job.action()
                            .automation_journal
                            .as_ref()
                            .is_some_and(|options| options.enabled),
                    )
                });
            entries = entries.push(
                button(job_row([
                    table_text(job.name()),
                    table_text(job.enabled().to_string()),
                    table_text(next),
                    table_text(dependencies),
                    table_text(journal_tokens),
                ]))
                .style(table_row_style(selected))
                .width(Length::Fill)
                .padding([5, 7])
                .clip(true)
                .on_press(Message::Select(job.id().clone())),
            );
        }
        let table = table(
            job_row(["Job", "Enabled", "Next run", "Depends on", "Journal AI"].map(table_text)),
            entries.into(),
        );
        let selected = runtime.selected_job(self);
        let runnable = runtime.can_request_run(self);
        let toolbar = row![
            button("➕  New")
                .style(button::secondary)
                .on_press(Message::New),
            button("✓  Save")
                .style(button::secondary)
                .on_press_maybe(runtime.can_edit().then_some(Message::Save)),
            button("✕  Delete").style(button::secondary).on_press_maybe(
                (runtime.can_edit() && selected.is_some()).then_some(Message::Delete)
            ),
            button("▶  Run now")
                .style(button::secondary)
                .on_press_maybe(runnable.then_some(Message::Run(false))),
            button("⏩  Run with successors")
                .style(button::secondary)
                .on_press_maybe(runnable.then_some(Message::Run(true))),
            button(if selected.is_some_and(|job| job.enabled()) {
                "⏻  Disable chain"
            } else {
                "⏻  Enable chain"
            })
            .style(button::secondary)
            .on_press_maybe(
                (runtime.can_edit() && selected.is_some()).then_some(Message::SwitchChain)
            ),
            button("🔀  Dependency diagram")
                .style(button::secondary)
                .on_press(Message::Page(Page::Diagram)),
        ]
        .spacing(8)
        .wrap();
        let mut panel = column![
            table,
            toolbar,
            checkbox(menu_status_visible)
                .label("Show Jobs status in menu bar")
                .on_toggle(Message::MenuStatusVisible),
        ]
        .spacing(10)
        .height(Length::Fill);
        if session_journal_root.is_some() && !graph.jobs().is_empty() {
            let status = match journal_stats {
                Some(Ok(report)) if report.incomplete => Some("Journal totals may be incomplete."),
                Some(Err(())) => Some("Could not read retained Session Journal totals."),
                None => Some("Loading retained Session Journal totals…"),
                Some(Ok(_)) => None,
            };
            if let Some(status) = status {
                panel = panel.push(text(status).size(13));
            }
        }
        if !runtime.execution_allowed {
            panel =
                panel.push(text("SSH job execution is disabled by application policy.").size(13));
        } else if runtime.has_active_runs() {
            panel =
                panel.push(text("Definitions can be saved when the current runs finish.").size(13));
        }
        if self.dirty {
            panel = panel.push(text("Unsaved changes").size(13));
        }
        if let Some(status) = &runtime.status {
            panel = panel.push(text(status).size(13));
        }
        if let Some(status) = &self.status {
            panel = panel.push(text(status).size(13));
        }
        container(panel).padding(8).height(Length::Fill).into()
    }

    fn details_view<'a>(
        &'a self,
        runtime: &'a SchedulerWindow,
        privileged_allowed: bool,
        session_journal_root: Option<&Path>,
    ) -> Element<'a, Message> {
        let mut tabs = row![].spacing(4).align_y(Alignment::Center);
        for (page, label) in [
            (Page::Job, "Job"),
            (Page::Action, "Action"),
            (Page::Journal, "Journal"),
        ] {
            tabs = tabs.push(
                button(label)
                    .style(tab_style(self.page == page))
                    .on_press(Message::Page(page)),
            );
        }
        let content: Element<'_, Message> = match self.page {
            Page::Job => scrollable(self.draft.job_view(
                runtime.scheduler.graph(),
                &runtime.connection_choices,
                &runtime.connection_group_choices,
                runtime.can_browse_directory(),
            ))
            .spacing(6)
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
            Page::Action => {
                let journal_activity = runtime.selected_job(self).and_then(|job| {
                    runtime
                        .journal_stats
                        .result(session_journal_root?)?
                        .as_ref()
                        .ok()?
                        .details(
                            job.id(),
                            job.action()
                                .automation_journal
                                .as_ref()
                                .is_some_and(|options| options.enabled),
                        )
                });
                let action = column![
                    journal_activity.map(|activity| container(text(activity).size(13)).padding(8)),
                    self.draft.action_view(
                        &runtime.snippet_choices,
                        &runtime.archive_passwords,
                        &runtime.webhook_choices,
                        &runtime.agent_profiles,
                        super::form::ActionPermissions {
                            browse: runtime.can_browse_directory(),
                            privileged: privileged_allowed,
                            automation_journal: !self.automation_journal_denied,
                        },
                    ),
                ];
                scrollable(action)
                    .spacing(6)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into()
            }
            Page::Journal => self.journal_view(runtime, session_journal_root),
            Page::Diagram => column![text("Dependency diagram").size(17), self.diagram.view(),]
                .spacing(8)
                .height(Length::Fill)
                .into(),
        };
        let mut body = column![tabs, rule::horizontal(1)].spacing(8);
        if let Some(confirm) = &self.confirm {
            body = body.push(self.confirm_view(runtime, confirm));
        } else if self.page == Page::Action
            && let Some(review) = self
                .agent_review
                .as_ref()
                .map(agent_review)
                .or_else(|| {
                    if privileged_allowed {
                        self.privileged_review.view()
                    } else {
                        None
                    }
                })
                .or_else(|| self.sync_review.view(runtime.scheduler.graph()))
                .or_else(|| self.rsync_review.view())
        {
            body = body.push(review);
        }
        container(body.push(content).height(Length::Fill))
            .padding(8)
            .height(Length::Fill)
            .into()
    }

    fn journal_view<'a>(
        &'a self,
        runtime: &'a SchedulerWindow,
        session_journal_root: Option<&Path>,
    ) -> Element<'a, Message> {
        let records: Vec<_> = runtime
            .store
            .journal()
            .into_iter()
            .filter(|record| self.matches_journal(runtime, record))
            .collect();
        let selected = records
            .iter()
            .copied()
            .find(|record| self.selected_run == Some(record.id));
        let contents = pane_grid(&self.layout.journal, |_, first, _| {
            let pane = if *first {
                self.journal_table(runtime, &records)
            } else {
                let detail = self
                    .selected_run
                    .filter(|&id| self.journal_active_visible(runtime, id))
                    .and_then(|id| {
                        let active = runtime.active.get(&id)?;
                        let proposals = runtime.active_proposals(id);
                        let name = runtime
                            .scheduler
                            .graph()
                            .job(&active.job)
                            .map_or(active.job.as_str(), |job| job.name());
                        (!proposals.is_empty())
                            .then(|| run_proposal::view(runtime, self, name, proposals))
                    })
                    .unwrap_or_else(|| {
                        let detail = selected.map_or_else(
                            || text("Select a run to see details or review a proposal.").into(),
                            |record| self.record_view(runtime, record, session_journal_root),
                        );
                        scrollable(detail).height(Length::Fill).into()
                    });
                container(detail)
                    .style(container::bordered_box)
                    .padding(8)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into()
            };
            pane_grid::Content::new(pane)
        })
        .spacing(8)
        .min_size(80)
        .on_resize(6, Message::ResizeJournal);
        column![
            self.journal_retention_view(runtime),
            self.journal_search_view(),
            contents,
            row![
                button(text(if self.marked_records.len() > 1 {
                    format!("✕  Delete selected ({})", self.marked_records.len())
                } else {
                    "✕  Delete selected".into()
                }))
                .style(button::secondary)
                .on_press_maybe(
                    (runtime.can_edit() && !self.marked_records.is_empty())
                        .then_some(Message::DeleteRecord)
                ),
                button("📓  Open session journals")
                    .style(button::secondary)
                    .on_press_maybe(selected.and_then(|record| {
                        (session_journal_root.is_some()
                            && runtime.store.session_journals(record.id).is_some_and(|ids| {
                                ids.len() <= crate::session_journal::automation::run_links::MAX_BULK_OPEN
                            }))
                        .then_some(Message::OpenSessionJournals(record.id))
                    })),
                button("Open virtual terminal recording")
                    .style(button::secondary)
                    .on_press_maybe(
                        selected
                            .filter(|record| runtime.store.journal_recording(record.id).is_some())
                            .map(|record| Message::OpenRecording(record.id))
                    ),
                button("Open Swarm conversation")
                    .style(button::secondary)
                    .on_press_maybe(selected.and_then(|record| {
                        runtime
                            .selected_swarm_chat(self, record.id)
                            .map(|_| Message::OpenSwarmChat(record.id))
                    })),
            ]
            .spacing(8)
            .wrap(),
        ]
        .spacing(10)
        .height(Length::Fill)
        .into()
    }

    fn journal_retention_view(&self, runtime: &SchedulerWindow) -> Element<'_, Message> {
        let current = runtime.journal_retention_days.to_string();
        let input = self
            .journal_retention_draft
            .as_ref()
            .map_or(current.as_str(), |(_, input)| input.as_str());
        let apply = self
            .journal_retention_change(runtime)
            .map(|_| Message::SaveJournalRetention);
        let controls = row![
            text("Run journal retention"),
            text_input("Days", input)
                .on_input(Message::EditJournalRetention)
                .on_submit_maybe(apply.clone())
                .width(66),
            text("days (0 disables age-based deletion)"),
            button("Apply").on_press_maybe(apply),
        ]
        .align_y(Alignment::Center)
        .spacing(8)
        .wrap();
        let mut contents = column![controls].spacing(4);
        if self.journal_retention_draft.is_some() && self.edited_journal_retention_days().is_none()
        {
            contents = contents.push(
                text(format!(
                    "Enter a whole number from 0 to {MAX_JOURNAL_RETENTION_DAYS}."
                ))
                .size(13),
            );
        }
        if let Some(error) = &runtime.journal_retention_error {
            contents = contents.push(text(error.clone()).size(13));
        }
        contents
            .push(
                self.journal_retention_java
                    .view(
                        runtime.journal_retention_days,
                        self.confirm.is_none() && !runtime.draining,
                    )
                    .map(Message::JournalRetentionJava),
            )
            .into()
    }

    fn journal_search_view(&self) -> Element<'_, Message> {
        let search = &self.journal_search;
        let mode = search.mode();
        let controls = row![
            text("Search"),
            text_input("Search terms with * as wildcard", search.text())
                .on_input(Message::JournalSearch),
            pick_list(
                JournalSearchMode::ALL,
                Some(mode),
                Message::JournalSearchMode
            ),
            button(text(if mode == JournalSearchMode::SelectedColumns {
                format!("Columns ({})", search.column_count())
            } else {
                "Columns".into()
            }))
            .style(button::secondary)
            .on_press_maybe(
                (mode == JournalSearchMode::SelectedColumns).then_some(Message::JournalColumns)
            ),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        let mut content = column![controls].spacing(6);
        if search.expanded() {
            let columns = JournalColumn::ALL
                .into_iter()
                .fold(row![].spacing(12), |row, column| {
                    row.push(
                        checkbox(search.includes(column))
                            .label(column.label())
                            .on_toggle(move |enabled| Message::JournalColumn(column, enabled)),
                    )
                });
            content = content.push(
                container(columns.wrap())
                    .padding(8)
                    .style(container::bordered_box),
            );
        }
        content.into()
    }

    pub(super) fn journal_record_visible(&self, runtime: &SchedulerWindow, id: RunId) -> bool {
        runtime
            .store
            .journal_record(id)
            .is_some_and(|record| self.matches_journal(runtime, record))
    }

    pub(super) fn reconcile_journal_selection(&mut self, runtime: &SchedulerWindow) {
        if let Some(selected) = self.selected_run
            && !self.journal_record_visible(runtime, selected)
            && !self.journal_active_visible(runtime, selected)
        {
            self.selected_run = None;
        }
        let mut marked = std::mem::take(&mut self.marked_records);
        marked.retain(|&id| self.journal_record_visible(runtime, id));
        self.marked_records = marked;
        if matches!(&self.confirm, Some(Confirmation::DeleteRecord(ids))
            if !self.journal_deletion_matches(runtime, ids))
        {
            self.confirm = None;
        }
    }

    pub(super) fn journal_deletion_matches(
        &self,
        runtime: &SchedulerWindow,
        ids: &[RunId],
    ) -> bool {
        self.page == Page::Journal
            && !ids.is_empty()
            && self.marked_records.iter().eq(ids)
            && ids
                .iter()
                .all(|&id| self.journal_record_visible(runtime, id))
    }

    pub(super) fn journal_active_visible(&self, runtime: &SchedulerWindow, id: RunId) -> bool {
        runtime.active.get(&id).is_some_and(|run| {
            let name = runtime
                .scheduler
                .graph()
                .job(&run.job)
                .map_or(run.job.as_str(), |job| job.name());
            let status = run_proposal::status(&runtime.active_proposals(id));
            self.matches_active_journal(id, run, name, &runtime.local_time(run.started_at), status)
        })
    }

    pub(super) fn journal_rows<'a>(
        &self,
        runtime: &'a SchedulerWindow,
        records: &[&'a RunRecord],
    ) -> Vec<JournalRow<'a>> {
        let mut rows = Vec::with_capacity(records.len() + runtime.active.len());
        for (&id, run) in runtime.active.iter().rev() {
            let name = runtime
                .scheduler
                .graph()
                .job(&run.job)
                .map_or(run.job.as_str(), |job| job.name());
            let started = runtime.local_time(run.started_at);
            let proposals = runtime.active_proposals(id);
            let status = run_proposal::status(&proposals);
            if self.matches_active_journal(id, run, name, &started, status) {
                rows.push(JournalRow {
                    id,
                    started_at: run.started_at,
                    started,
                    status,
                    name,
                    session_journals: 0,
                    summary: Cow::Borrowed(""),
                    active: Some(proposals),
                });
            }
        }
        for &run in records {
            let session_journals = runtime.store.session_journals(run.id).map_or(0, <[_]>::len);
            let summary = runtime.store.journal_swarm_report(run.id).map_or_else(
                || Cow::Borrowed(runtime.store.journal_detail(run.id).unwrap_or_default()),
                |report| Cow::Owned(swarm_report::summary(report)),
            );
            rows.push(JournalRow {
                id: run.id,
                started_at: run.started_at,
                started: runtime.local_time(run.started_at),
                status: outcome(run.outcome),
                name: run.job_name.as_str(),
                session_journals,
                summary,
                active: None,
            });
        }
        self.journal_sort.order(&mut rows);
        rows
    }

    fn journal_table<'a>(
        &'a self,
        runtime: &'a SchedulerWindow,
        records: &[&'a RunRecord],
    ) -> Element<'a, Message> {
        let mut entries = column![].spacing(1);
        for row in self.journal_rows(runtime, records) {
            let JournalRow {
                id,
                started,
                status,
                name,
                session_journals,
                summary,
                active,
                ..
            } = row;
            let selected = self.selected_run == Some(id);
            let journals = if session_journals == 0 {
                String::new()
            } else {
                format!("{session_journals} linked")
            };
            let [started, status, name, journals] = [
                table_text(started),
                table_text(status),
                table_text(name),
                table_text(journals),
            ];
            let entry = if let Some(proposals) = active {
                let mut actions = row![].spacing(4);
                if !proposals.is_empty() {
                    let label = if proposals
                        .iter()
                        .any(|proposal| matches!(proposal, RunProposalView::Awaiting { .. }))
                    {
                        "Review"
                    } else {
                        "Details"
                    };
                    actions = actions.push(
                        button(text(label).size(12))
                            .padding([2, 5])
                            .style(table_row_style(selected))
                            .on_press(Message::SelectActiveRun(id)),
                    );
                }
                let actions = actions.push(
                    button(text("Cancel").size(12))
                        .padding([2, 5])
                        .style(button::secondary)
                        .on_press(Message::CancelRun(id)),
                );
                journal_row(
                    text("").into(),
                    journal_cells([started, status, name, journals, actions.into()]),
                )
            } else {
                let summary = match summary {
                    Cow::Borrowed(summary) => table_text(summary),
                    Cow::Owned(summary) => table_text(summary),
                };
                journal_row(
                    checkbox(self.marked_records.contains(&id))
                        .size(14)
                        .on_toggle(move |marked| Message::MarkRecord(id, marked))
                        .into(),
                    button(journal_cells([started, status, name, journals, summary]))
                        .style(table_row_style(selected))
                        .width(Length::Fill)
                        .padding(0)
                        .clip(true)
                        .on_press(Message::SelectRecord(id))
                        .into(),
                )
            };
            entries = entries.push(container(entry).padding([5, 7]));
        }
        table(
            journal_row(
                text("").into(),
                journal_cells(
                    JournalSortColumn::ALL.map(|column| self.journal_sort.header(column)),
                ),
            ),
            entries.into(),
        )
    }

    fn matches_active_journal(
        &self,
        id: RunId,
        run: &super::Active,
        name: &str,
        started: &str,
        status: &str,
    ) -> bool {
        use JournalColumn::*;
        if self.journal_search.is_empty() {
            return true;
        }
        let id = id.to_string();
        let raw_started = journal_timestamp(run.started_at);
        self.journal_search.matches([
            (Some(Started), started),
            (Some(Finished), ""),
            (Some(Status), status),
            (Some(Job), name),
            (Some(Summary), ""),
            (Some(Detail), ""),
            (Some(Trigger), trigger(run.trigger)),
            (Some(RunId), id.as_str()),
            (Some(ExitCode), ""),
            (None, run.job.as_str()),
            (None, raw_started.as_str()),
        ])
    }

    fn matches_journal(&self, runtime: &SchedulerWindow, run: &RunRecord) -> bool {
        use JournalColumn::*;
        let search = &self.journal_search;
        if search.is_empty() {
            return true;
        }
        let started = runtime.local_time(run.started_at);
        let finished = runtime.local_time(run.finished_at);
        let id = run.id.to_string();
        let exit_code = match run.outcome {
            RunOutcome::Failed {
                exit_code: Some(code),
            } => code.to_string(),
            _ => String::new(),
        };
        let summary = runtime.store.journal_detail(run.id).unwrap_or_default();
        let report_summary = runtime
            .store
            .journal_swarm_report(run.id)
            .filter(|_| search.searches(Summary))
            .map(swarm_report::summary);
        let raw_started = journal_timestamp(run.started_at);
        let raw_finished = journal_timestamp(run.finished_at);
        let blockers: Vec<_> = if search.searches(Detail) {
            run.blockers
                .iter()
                .map(|blocker| Self::blocker(runtime, blocker))
                .collect()
        } else {
            Vec::new()
        };
        search.matches(
            [
                (Some(Started), started.as_str()),
                (Some(Finished), finished.as_str()),
                (Some(Status), outcome(run.outcome)),
                (Some(Job), run.job_name.as_str()),
                (Some(Summary), summary),
                (Some(Summary), report_summary.as_deref().unwrap_or_default()),
                (Some(Detail), summary),
                (Some(Trigger), trigger(run.trigger)),
                (Some(RunId), id.as_str()),
                (Some(ExitCode), exit_code.as_str()),
                (None, run.job.as_str()),
                (None, raw_started.as_str()),
                (None, raw_finished.as_str()),
            ]
            .into_iter()
            .chain(
                blockers
                    .iter()
                    .map(|blocker| (Some(Detail), blocker.as_str())),
            ),
        )
    }

    fn record_view<'a>(
        &'a self,
        runtime: &'a SchedulerWindow,
        record: &'a RunRecord,
        session_journal_root: Option<&Path>,
    ) -> Element<'a, Message> {
        let mut details = column![
            text(&record.job_name).size(19),
            text(format!(
                "{} · {}",
                outcome(record.outcome),
                trigger(record.trigger)
            )),
            text(format!("Started {}", runtime.local_time(record.started_at))),
            text(format!(
                "Finished {}",
                runtime.local_time(record.finished_at)
            )),
        ]
        .spacing(6);
        if let RunOutcome::Failed {
            exit_code: Some(code),
        } = record.outcome
        {
            details = details.push(text(format!("Exit status {code}")));
        }
        if let Some(report) = runtime.store.journal_swarm_report(record.id) {
            details = details.push(swarm_report::view(report));
        }
        if let Some(detail) = runtime.store.journal_detail(record.id) {
            details = details.push(text(detail));
        }
        if let Some(root) = session_journal_root
            && runtime.store.session_journals(record.id).is_some()
        {
            details = details.push(journal_details::view(
                self.journal_details(record.id, root),
                runtime,
                record.id,
            ));
        }
        if let Some(results) = runtime.store.webhook_results(record.id) {
            details = details.push(text("Webhook delivery").size(15));
            for (id, result) in results {
                let name = runtime
                    .webhook_choices
                    .iter()
                    .find(|choice| &choice.id == id)
                    .map_or("Vault destination", |choice| choice.label.as_str());
                let outcome = super::webhooks::outcome_text(*result);
                details = details.push(text(format!("{name} · {outcome}")));
            }
        }
        for blocker in &record.blockers {
            details = details.push(text(Self::blocker(runtime, blocker)));
        }
        details.into()
    }

    fn confirm_view<'a>(
        &'a self,
        runtime: &'a SchedulerWindow,
        confirm: &Confirmation,
    ) -> Element<'a, Message> {
        let (accept, cancel) = match confirm {
            Confirmation::Close => (
                button("Discard changes").on_press(
                    self.hosted_close.map_or(Message::CloseConfirmed, |token| {
                        Message::HostedClose(token, true)
                    }),
                ),
                button("Keep editing").on_press(
                    self.hosted_close
                        .map_or(Message::Dismiss, |token| Message::HostedClose(token, false)),
                ),
            ),
            Confirmation::SyncCascade(_, _) => (
                button("Start reviewed cascade").on_press(Message::Confirm),
                button("Cancel").on_press(Message::Dismiss),
            ),
            Confirmation::DeleteRecord(_) => (
                button("Delete selected")
                    .style(button::danger)
                    .on_press(Message::Confirm),
                button("Cancel").on_press(Message::Dismiss),
            ),
            _ => (
                button("Continue").on_press(Message::Confirm),
                button("Cancel").on_press(Message::Dismiss),
            ),
        };
        let body = match confirm {
            Confirmation::Close => column![text("Discard unsaved job changes?")],
            Confirmation::Run(_, cascade, blockers) => {
                let mut body = column![text(if *cascade {
                    "Start this job despite these conditions and continue to eligible successors after success?"
                } else {
                    "Start this job despite these conditions?"
                })];
                for blocker in blockers {
                    body = body.push(text(Self::blocker(runtime, blocker)));
                }
                body
            }
            Confirmation::SyncCascade(review, blockers) => {
                let graph = runtime.scheduler.graph();
                let root = graph
                    .job(review.root())
                    .map_or(review.root().as_str(), |job| job.name());
                let mut body = column![
                    text(format!("Start {root} and its eligible successors?")),
                    text("This confirmation applies to one cascade. Eligible SFTP copies use their existing one-use folder approvals.").size(13),
                    text("Other jobs can still start through their own schedules or Chain approvals.").size(12),
                ];
                if !blockers.is_empty() {
                    body = body.push(
                        text("Only the selected start job may bypass these conditions.").size(13),
                    );
                    for blocker in blockers {
                        body = body.push(text(Self::blocker(runtime, blocker)));
                    }
                }
                let mut copies = column![].spacing(8);
                for (id, available) in review.copies() {
                    let Some(job) = graph.job(id) else { continue };
                    let role = if id == review.root() {
                        "Start job"
                    } else {
                        "Successor"
                    };
                    let mut details =
                        column![text(format!("{role} · {} · {id}", job.name()))].spacing(2);
                    let action = job.action().remote_action();
                    if let Ok(RemoteAction::Sync(copy)) = &action {
                        let direction = match copy.direction {
                            sync::Direction::Upload => "Upload",
                            sync::Direction::Download => "Download",
                        };
                        if let Some(route) = &job.action().connection {
                            let target = route.target.label();
                            let route = route.jump.as_ref().map_or_else(
                                || target.clone(),
                                |jump| format!("{} via {}", target, jump.label()),
                            );
                            details = details.push(text(format!("{direction} · {route}")).size(12));
                        }
                        details = details
                            .push(text(format!("Local · {}", copy.local.display())).size(12))
                            .push(text(format!("Server · {}", copy.remote.as_str())).size(12));
                    }
                    details = details.push(
                        text(if available {
                            "Ready to use the saved folder approval."
                        } else if let Some(reason) = review.unavailable_reason(id) {
                            reason
                        } else if matches!(action, Ok(RemoteAction::PrivilegedTreeUpload(_) | RemoteAction::PrivilegedTreeDownload(_))) {
                            "Start this privileged transfer separately. The branch stops if it reaches this job."
                        } else {
                            "Unavailable for this cascade. The branch stops if it reaches this job."
                        })
                        .size(12),
                    );
                    copies = copies.push(details);
                }
                body.push(
                    scrollable(copies)
                        .width(Length::Fill)
                        .height(Length::Fixed(164.0)),
                )
            }
            Confirmation::Delete(_, dependents) => {
                let mut body = column![text("Delete the job? Its run history will remain.")];
                if !dependents.is_empty() {
                    body = body.push(text(format!(
                        "These jobs will drop the dependency ({})",
                        dependents.join(", ")
                    )));
                }
                body
            }
            Confirmation::SwitchChain(_, enabled) => column![text(if *enabled {
                "Enable the selected job and all of its successors?"
            } else {
                "Disable the selected job and all of its successors?"
            })],
            Confirmation::DeleteRecord(ids) => column![text(if ids.len() == 1 {
                "Delete this journal entry?".into()
            } else {
                format!("Delete {} selected journal entries?", ids.len())
            })],
        };
        container(body.push(row![accept, cancel].spacing(7)).spacing(6))
            .padding(8)
            .style(container::bordered_box)
            .width(Length::Fill)
            .into()
    }

    fn blocker(runtime: &SchedulerWindow, blocker: &Blocker) -> String {
        let name = runtime
            .scheduler
            .graph()
            .job(&blocker.predecessor)
            .map_or(blocker.predecessor.as_str(), |job| job.name());
        let reason = match blocker.reason {
            BlockReason::NeverRan => "has never run",
            BlockReason::NotSuccessful(_) => "did not succeed",
            BlockReason::TooOld => "last succeeded outside the allowed age",
            BlockReason::NotSinceLastRun => "has not succeeded since this job last ran",
        };
        format!("{name} {reason}")
    }
}

fn table_text<'a>(content: impl text::IntoFragment<'a>) -> Element<'a, Message> {
    text(content).size(13).wrapping(text::Wrapping::None).into()
}

fn job_row(cells: [Element<'_, Message>; 5]) -> Element<'_, Message> {
    columns(cells, [4, 2, 4, 3, 3])
}

fn journal_cells(cells: [Element<'_, Message>; 5]) -> Element<'_, Message> {
    columns(cells, [4, 2, 3, 3, 5])
}

fn journal_row<'a>(
    selection: Element<'a, Message>,
    cells: Element<'a, Message>,
) -> Element<'a, Message> {
    row![container(selection).width(22), cells]
        .spacing(7)
        .align_y(Alignment::Center)
        .width(Length::Fill)
        .into()
}

fn columns<const N: usize>(
    cells: [Element<'_, Message>; N],
    widths: [u16; N],
) -> Element<'_, Message> {
    cells
        .into_iter()
        .zip(widths)
        .fold(
            row![].spacing(7).align_y(Alignment::Center),
            |row, (cell, width)| {
                row.push(
                    container(cell)
                        .width(Length::FillPortion(width))
                        .height(22)
                        .align_y(Alignment::Center)
                        .clip(true),
                )
            },
        )
        .width(Length::Fill)
        .into()
}

fn table<'a>(header: Element<'a, Message>, entries: Element<'a, Message>) -> Element<'a, Message> {
    container(
        column![
            container(header)
                .padding([5, 7])
                .style(container::secondary),
            scrollable(entries).width(Length::Fill).height(Length::Fill),
        ]
        .height(Length::Fill),
    )
    .style(container::bordered_box)
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn table_row_style(active: bool) -> fn(&iced::Theme, button::Status) -> button::Style {
    if active {
        button::primary
    } else {
        button::subtle
    }
}

fn journal_timestamp(time: SystemTime) -> String {
    jiff::Timestamp::try_from(time).map_or_else(|_| String::new(), |time| time.to_string())
}

fn outcome(outcome: RunOutcome) -> &'static str {
    match outcome {
        RunOutcome::Success => "Successful",
        RunOutcome::Failed { .. } => "Failed",
        RunOutcome::Blocked => "Blocked",
        RunOutcome::Cancelled => "Cancelled",
    }
}

fn trigger(trigger: RunTrigger) -> &'static str {
    match trigger {
        RunTrigger::Manual => "Manual run",
        RunTrigger::Scheduled => "Scheduled run",
        RunTrigger::Dependency => "Chain run",
        RunTrigger::Cascade => "Run with successors",
    }
}

fn tab_style(active: bool) -> fn(&iced::Theme, button::Status) -> button::Style {
    if active {
        button::primary
    } else {
        button::secondary
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduler_view_uses_reference_dividers() {
        let layout = Layout::default();
        for (state, expected_axis, expected_ratio) in [
            (&layout.jobs, pane_grid::Axis::Vertical, 0.27),
            (&layout.journal, pane_grid::Axis::Horizontal, 0.72),
        ] {
            let pane_grid::Node::Split { axis, ratio, .. } = state.layout() else {
                panic!("Scheduler requires two panes");
            };
            assert_eq!((*axis, *ratio), (expected_axis, expected_ratio));
        }
    }
}
