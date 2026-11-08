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
use super::{Confirmation, Message, Page, SchedulerView, SchedulerWindow, Scope, SshCommand};
use crate::diagram_window::Format;

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
    ) -> Element<'a, Message> {
        if let Some(confirm @ Confirmation::Close) = &self.confirm {
            return container(self.confirm_view(runtime, confirm))
                .padding(12)
                .center(Length::Fill)
                .into();
        }
        let contents = pane_grid(&self.layout.jobs, |_, first, _| {
            pane_grid::Content::new(if *first {
                self.jobs_view(runtime, menu_status_visible)
            } else {
                self.details_view(runtime)
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
    ) -> Element<'a, Message> {
        let graph = runtime.scheduler.graph();
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
            entries = entries.push(
                button(job_row([
                    table_text(job.name()),
                    table_text(job.enabled().to_string()),
                    table_text(next),
                    table_text(dependencies),
                ]))
                .style(table_row_style(selected))
                .width(Length::Fill)
                .padding([5, 7])
                .clip(true)
                .on_press(Message::Select(job.id().clone())),
            );
        }
        let table = table(
            job_row(["Job", "Enabled", "Next run", "Depends on"].map(table_text)),
            entries.into(),
        );
        let selected = runtime.selected_job(self);
        let runnable = selected.is_some_and(|job| !runtime.scheduler.is_running(job.id()))
            && runtime.can_run()
            && !self.dirty;
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

    fn details_view<'a>(&'a self, runtime: &'a SchedulerWindow) -> Element<'a, Message> {
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
            Page::Job => scrollable(
                self.draft
                    .job_view(runtime.scheduler.graph(), &runtime.connection_choices),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
            Page::Action => scrollable(
                self.draft
                    .action_view(&runtime.snippet_choices, &runtime.archive_passwords),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
            Page::Journal => self.journal_view(runtime),
            Page::Diagram => column![text("Dependency diagram").size(17), self.diagram.view(),]
                .spacing(8)
                .height(Length::Fill)
                .into(),
        };
        let mut body = column![tabs, rule::horizontal(1)].spacing(8);
        if let Some(confirm) = &self.confirm {
            body = body.push(self.confirm_view(runtime, confirm));
        }
        container(body.push(content).height(Length::Fill))
            .padding(8)
            .height(Length::Fill)
            .into()
    }

    fn journal_view<'a>(&'a self, runtime: &'a SchedulerWindow) -> Element<'a, Message> {
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
                let detail = selected.map_or_else(
                    || text("Select a finished run to see details.").into(),
                    |record| self.record_view(runtime, record),
                );
                container(scrollable(detail).height(Length::Fill))
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
            self.journal_search_view(),
            contents,
            row![
                button("✕  Delete selected")
                    .style(button::secondary)
                    .on_press_maybe(
                        (runtime.can_edit() && selected.is_some()).then_some(Message::DeleteRecord)
                    ),
                button("📓  Open session journal")
                    .style(button::secondary)
                    .on_press_maybe(
                        selected
                            .filter(|record| runtime.store.journal_recording(record.id).is_some())
                            .map(|record| Message::OpenRecording(record.id))
                    ),
            ]
            .spacing(8)
            .wrap(),
        ]
        .spacing(10)
        .height(Length::Fill)
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
        {
            self.selected_run = None;
        }
        if matches!(self.confirm, Some(Confirmation::DeleteRecord(id))
            if self.page != Page::Journal || self.selected_run != Some(id))
        {
            self.confirm = None;
        }
    }

    pub(super) fn journal_active_visible(&self, runtime: &SchedulerWindow, id: RunId) -> bool {
        runtime.active.get(&id).is_some_and(|run| {
            let name = runtime
                .scheduler
                .graph()
                .job(&run.job)
                .map_or(run.job.as_str(), |job| job.name());
            self.matches_active_journal(id, run, name, &runtime.local_time(run.started_at))
        })
    }

    fn journal_table<'a>(
        &'a self,
        runtime: &'a SchedulerWindow,
        records: &[&'a RunRecord],
    ) -> Element<'a, Message> {
        let mut entries = column![].spacing(1);
        for (&id, run) in runtime.active.iter().rev() {
            let name = runtime
                .scheduler
                .graph()
                .job(&run.job)
                .map_or(run.job.as_str(), |job| job.name());
            let started = runtime.local_time(run.started_at);
            if !self.matches_active_journal(id, run, name, &started) {
                continue;
            }
            entries = entries.push(
                container(journal_row([
                    table_text(started),
                    table_text("Running"),
                    table_text(name),
                    table_text(""),
                    button(text("Cancel").size(12))
                        .padding([2, 5])
                        .style(button::secondary)
                        .on_press(Message::CancelRun(id))
                        .into(),
                ]))
                .padding([3, 7]),
            );
        }
        for run in records {
            let selected = self.selected_run == Some(run.id);
            let recording = if runtime.store.journal_recording(run.id).is_some() {
                "Available"
            } else {
                ""
            };
            entries = entries.push(
                button(journal_row([
                    table_text(runtime.local_time(run.started_at)),
                    table_text(outcome(run.outcome)),
                    table_text(run.job_name.as_str()),
                    table_text(recording),
                    table_text(runtime.store.journal_detail(run.id).unwrap_or_default()),
                ]))
                .style(table_row_style(selected))
                .width(Length::Fill)
                .padding([5, 7])
                .clip(true)
                .on_press(Message::SelectRecord(run.id)),
            );
        }
        table(
            journal_row(["Started", "Status", "Job", "Session journal", "Summary"].map(table_text)),
            entries.into(),
        )
    }

    fn matches_active_journal(
        &self,
        id: RunId,
        run: &super::Active,
        name: &str,
        started: &str,
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
            (Some(Status), "Running"),
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
        if let Some(detail) = runtime.store.journal_detail(record.id) {
            details = details.push(text(detail));
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
            _ => (
                button("Continue").on_press(Message::Confirm),
                button("Cancel").on_press(Message::Dismiss),
            ),
        };
        let body = match confirm {
            Confirmation::Close => column![text("Discard unsaved job changes?")],
            Confirmation::Run(_, cascade, blockers) => {
                let mut body = column![text(if *cascade {
                    "Start this job and its successors despite these conditions?"
                } else {
                    "Start this job despite these conditions?"
                })];
                for blocker in blockers {
                    body = body.push(text(Self::blocker(runtime, blocker)));
                }
                body
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
            Confirmation::DeleteRecord(_) => column![text("Delete this journal entry?")],
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

fn job_row(cells: [Element<'_, Message>; 4]) -> Element<'_, Message> {
    columns(cells, [4, 2, 4, 3])
}

fn journal_row(cells: [Element<'_, Message>; 5]) -> Element<'_, Message> {
    columns(cells, [4, 2, 3, 3, 5])
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
