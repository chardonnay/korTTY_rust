//! Advisory planning controls and explicit saved-plan persistence.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use iced::widget::{button, column, row, scrollable, text, text_editor};
use iced::{Element, Length, Task, window};

use crate::terminal_agent::planning::store::{self, Destination, Record};
use crate::terminal_agent::planning::{self as model, Approach, Question, Reply, Report, Stage};
use crate::terminal_agent::planning_probe;
use crate::terminal_agent::usage;

use super::request_review::{Draft, Review, mask_json_strings};
use super::{
    Desktop, MAX_RUNS, Message as RunMessage, Mode, Outcome, Phase, Run, RunTarget, Workspace,
    json, mask,
};

#[cfg(test)]
mod tests;

const MAX_REFINEMENT_BYTES: usize = 2048;
const MAX_CUSTOM_ANSWER_BYTES: usize = 1024;

#[derive(Clone)]
pub enum Message {
    SelectAnswer(u64, usize, usize),
    EditCustomAnswer(u64, usize, text_editor::Action),
    SubmitAnswers(u64),
    EditAnswers(u64),
    ReturnToResults(u64),
    EditRefinement(u64, text_editor::Action),
    Revise(u64),
    Choose(u64, usize),
    PrepareImplementation(u64, usize),
    KeepReport(u64),
    Progress(u64, usage::Progress),
    Reply(u64, Arc<usage::Report<Reply>>),
    Save,
    Browse,
    Open(u64, String),
    Delete(u64, String),
    ConfirmDelete(u64, bool),
    DismissLibrary,
    Storage(u64, Arc<Result<Vec<Record>, String>>),
}

#[derive(Default)]
struct AnswerDraft {
    selected: usize,
    custom: text_editor::Content,
}

pub(super) struct Flow {
    destination: Destination,
    probe: Option<planning_probe::Snapshot>,
    questions: Vec<Question>,
    answers: Vec<String>,
    answer_drafts: Vec<AnswerDraft>,
    pending_answers: Option<Vec<String>>,
    refinement: text_editor::Content,
    options: Vec<Approach>,
    chosen: Option<usize>,
    review_implementation: bool,
    report: Option<Report>,
    records: Vec<Record>,
    deleting: Option<Record>,
    saved: bool,
    storage_from: Phase,
}

impl Flow {
    fn new(destination: Destination) -> Self {
        Self {
            destination,
            probe: None,
            questions: Vec::new(),
            answers: Vec::new(),
            answer_drafts: Vec::new(),
            pending_answers: None,
            refinement: text_editor::Content::new(),
            options: Vec::new(),
            chosen: None,
            review_implementation: false,
            report: None,
            records: Vec::new(),
            deleting: None,
            saved: false,
            storage_from: Phase::Setup,
        }
    }

    pub(super) fn probe_summary(&self) -> Option<String> {
        self.probe.as_ref().map(planning_probe::Snapshot::summary)
    }

    pub(super) fn view<'a>(&'a self, run: &'a Run) -> Option<Element<'a, RunMessage>> {
        let message = RunMessage::Planning;
        let enabled = !run.paused && !run.cancel.requested();
        let size = run.fonts.plan;
        match run.phase {
            Phase::PlanQuestions => Some(self.question_form(run, enabled)),
            Phase::PlanOptions => {
                Some(scrollable(self.approaches(run)).height(Length::Fill).into())
            }
            Phase::PlanReady | Phase::PlanImplementationReview | Phase::PlanHandedOff => {
                let report = run.plan.as_deref()?;
                Some(
                    scrollable(
                        column![
                            text(report).size(size).width(Length::Fill),
                            (run.phase == Phase::PlanReady
                                && self.report.is_some()
                                && !run.binding.private())
                            .then(|| {
                                button(if self.saved {
                                    "Plan saved"
                                } else {
                                    "Save plan"
                                })
                                .on_press_maybe(
                                    (enabled && !self.saved).then_some(message(Message::Save)),
                                )
                            }),
                            (run.phase == Phase::PlanReady && !self.options.is_empty())
                                .then(|| self.approaches(run)),
                        ]
                        .spacing(12),
                    )
                    .height(Length::Fill)
                    .into(),
                )
            }
            Phase::PlanLibrary => {
                let mut records = column![
                    text(format!("Saved plans for {}", self.destination.label())).size(size)
                ]
                .spacing(6);
                for record in &self.records {
                    records = records.push(
                        row![
                            button(text(&record.report.title).size(size))
                                .on_press(message(Message::Open(run.epoch, record.id.clone()))),
                            button("Delete…")
                                .on_press(message(Message::Delete(run.epoch, record.id.clone()))),
                        ]
                        .spacing(6),
                    );
                }
                if self.records.is_empty() {
                    records = records
                        .push(text("There are no saved plans for this exact route.").size(size));
                }
                if let Some(record) = &self.deleting {
                    records = records.push(
                        row![
                            text(format!("Delete ‘{}’?", record.report.title)).size(size),
                            button("Delete plan")
                                .on_press(message(Message::ConfirmDelete(run.epoch, true))),
                            button("Keep plan")
                                .on_press(message(Message::ConfirmDelete(run.epoch, false))),
                        ]
                        .spacing(6),
                    );
                }
                Some(
                    records
                        .push(button("Back").on_press(message(Message::DismissLibrary)))
                        .into(),
                )
            }
            _ => None,
        }
    }

    fn question_form<'a>(&'a self, run: &'a Run, enabled: bool) -> Element<'a, RunMessage> {
        let epoch = run.epoch;
        let size = run.fonts.plan;
        let mut questions = column![].spacing(12);
        for (index, (question, draft)) in self.questions.iter().zip(&self.answer_drafts).enumerate()
        {
            let mut choices = row![].spacing(6);
            for (option, answer) in question.options.iter().enumerate() {
                choices = choices.push(
                    button(text(answer).size(size))
                        .style(if draft.selected == option {
                            button::primary
                        } else {
                            button::secondary
                        })
                        .on_press_maybe(enabled.then_some(RunMessage::Planning(
                            Message::SelectAnswer(epoch, index, option),
                        ))),
                );
            }
            let mut fields =
                column![text(&question.question).size(size), choices.wrap()].spacing(6);
            if question.allow_custom_answer {
                let editor = text_editor(&draft.custom)
                    .placeholder("Additional details (optional)")
                    .size(size)
                    .height(70);
                fields = fields.push(if enabled {
                    editor.on_action(move |action| {
                        RunMessage::Planning(Message::EditCustomAnswer(epoch, index, action))
                    })
                } else {
                    editor
                });
            }
            questions = questions.push(fields);
        }
        questions = questions.push(
            row![
                button("Review answers…").on_press_maybe(
                    enabled.then_some(RunMessage::Planning(Message::SubmitAnswers(epoch)))
                ),
                self.previous_results().map(|phase| {
                    button(if phase == Phase::PlanReady {
                        "Back to plan"
                    } else {
                        "Back to approaches"
                    })
                    .on_press_maybe(
                        enabled.then_some(RunMessage::Planning(Message::ReturnToResults(epoch))),
                    )
                }),
            ]
            .spacing(6),
        );
        scrollable(questions).height(Length::Fill).into()
    }

    fn approaches<'a>(&'a self, run: &'a Run) -> Element<'a, RunMessage> {
        let epoch = run.epoch;
        let size = run.fonts.plan;
        let enabled = !run.paused && !run.cancel.requested();
        let editor = text_editor(&self.refinement)
            .placeholder("Additional requirements for the next request")
            .size(size)
            .height(86);
        let editor = if enabled {
            editor.on_action(move |action| {
                RunMessage::Planning(Message::EditRefinement(epoch, action))
            })
        } else {
            editor
        };
        let mut options = column![
            text("Choose an implementation approach").size(size),
            (!self.questions.is_empty()).then(|| {
                button("Edit answers…").on_press_maybe(
                    enabled.then_some(RunMessage::Planning(Message::EditAnswers(epoch))),
                )
            }),
            editor,
            button("Review revised approaches…")
                .on_press_maybe(enabled.then_some(RunMessage::Planning(Message::Revise(epoch)))),
        ]
        .spacing(8);
        for (index, option) in self.options.iter().enumerate() {
            options = options.push(
                column![
                    text(&option.title).size(size + 3.0),
                    text(option.render()).size(size),
                    row![
                        button("Draft plan").on_press_maybe(
                            enabled.then_some(RunMessage::Planning(Message::Choose(epoch, index))),
                        ),
                        button("Implement this approach…").on_press_maybe(enabled.then_some(
                            RunMessage::Planning(Message::PrepareImplementation(epoch, index)),
                        )),
                    ]
                    .spacing(6)
                    .wrap(),
                ]
                .spacing(4),
            );
        }
        options.into()
    }

    fn previous_results(&self) -> Option<Phase> {
        (!self.options.is_empty()).then_some(if self.report.is_some() {
            Phase::PlanReady
        } else {
            Phase::PlanOptions
        })
    }

    fn can_answer(&self, run: &Run, epoch: u64) -> bool {
        run.phase == Phase::PlanQuestions && run.epoch == epoch && !run.paused
    }

    fn can_refine(&self, run: &Run, epoch: u64) -> bool {
        matches!(run.phase, Phase::PlanOptions | Phase::PlanReady)
            && run.epoch == epoch
            && !run.paused
            && !self.options.is_empty()
    }
}

impl Workspace {
    pub(super) fn implement_agent_plan(
        &mut self,
        planner: window::Id,
        epoch: u64,
    ) -> Task<Desktop> {
        let Some(plan) = self.terminal_agents.runs.get(&planner) else {
            return Task::none();
        };
        if plan.mode != Mode::Plan
            || !matches!(
                plan.phase,
                Phase::PlanReady | Phase::PlanImplementationReview
            )
            || plan.epoch != epoch
            || plan.swarm
            || plan.paused
            || plan.closing
            || plan.cancel.requested()
        {
            return Task::none();
        }
        if self.terminal_agents.runs.len() >= MAX_RUNS {
            self.terminal_agents
                .runs
                .get_mut(&planner)
                .expect("reviewed plan")
                .notice = "Close an agent activity before implementing the plan.".into();
            return Task::none();
        }
        let mut plan = self
            .terminal_agents
            .runs
            .remove(&planner)
            .expect("reviewed plan");
        let task = match self.prepare_accepted_plan(&plan) {
            Ok((owner, run)) => {
                let (id, opened) = self.open_agent_surface(owner, &run);
                self.terminal_agents.runs.insert(id, run);
                let started = self.update_terminal_agent(id, RunMessage::Start);
                if self.terminal_agents.runs[&id].active() {
                    plan.next(
                        Phase::PlanHandedOff,
                        "Implementation opened in a separate Agent view.",
                    );
                    self.terminal_agents.activity.capture(&mut plan);
                }
                opened.chain(started)
            }
            Err(error) => {
                let before = plan.phase;
                plan.block(error);
                match (plan.binding.live(), plan.phase.notice(before)) {
                    (Ok(source), Some(notice)) if !source.private => {
                        self.notify_agent_run(planner, source.session, source.stream, notice)
                    }
                    _ => Task::none(),
                }
            }
        };
        self.terminal_agents.runs.insert(planner, plan);
        task
    }

    fn prepare_accepted_plan(&self, plan: &Run) -> Result<(window::Id, Run), String> {
        self.check_run_destination(plan)?;
        let source = plan.binding.live()?;
        let (owner, _) = self.current_agent_pane(source, Mode::Run)?;
        self.validate_plan_destination(plan)?;
        if plan.target.is_some() {
            self.planning_probe_target(plan)?;
        }
        let profile = plan
            .profile()
            .ok_or("The planning profile is unavailable.")?;
        let mut run = self.build_agent(source.session, Mode::Run, None, Some(profile.id), false)?;
        if run.binding.live()? != source || run.profile() != Some(profile) {
            return Err("The source pane or planning profile changed. Review a new plan.".into());
        }
        run.plan = Some(
            plan.plan
                .clone()
                .ok_or("The reviewed plan is unavailable.")?,
        );
        run.goal.clone_from(&plan.goal);
        run.manual_directory.clone_from(&plan.manual_directory);
        run.target.clone_from(&plan.target);
        run.surface = self.agent_run_presentation();
        run.chat_handoff_enabled = self.ai_chat_enabled();
        Ok((owner, run))
    }

    pub(super) fn planning_start(
        &self,
        id: window::Id,
        run: &mut Run,
        direct: bool,
        with_probe: bool,
    ) -> Result<Task<Desktop>, String> {
        run.planning = Some(Flow::new(self.planning_destination(run)?));
        let (_, pane) = self.current_run_pane(run)?;
        if with_probe
            && self.agent_policy(pane, Mode::Run).is_ok()
            && (pane.ssh.is_some() || pane.session.local_shell_running())
        {
            return match self.prepare_attached_probe(id, run, direct) {
                Ok(task) => Ok(task),
                Err(error) => self.plan_without_probe_after_failure(id, run, &error),
            };
        }
        self.planning_questions(id, run)
    }

    pub(super) fn planning_questions(
        &self,
        id: window::Id,
        run: &mut Run,
    ) -> Result<Task<Desktop>, String> {
        let flow = run
            .planning
            .take()
            .ok_or("The planning destination is unavailable.")?;
        let result = self.prepare_plan_review(id, run, &flow, Stage::Questions);
        run.planning = Some(flow);
        result
    }

    pub(super) fn finish_planning_probe(
        &self,
        id: window::Id,
        run: &mut Run,
        result: &Result<Outcome, String>,
    ) -> Result<Task<Desktop>, String> {
        self.require_plan_review_route(run)?;
        self.planning_probe_target(run)?;
        let Some(RunTarget::Attached(target)) = &run.target else {
            return Err("The reviewed probe destination is unavailable.".into());
        };
        let snapshot = result.as_ref().map_err(Clone::clone).and_then(|result| {
            planning_probe::Snapshot::capture(target, result, &|text| {
                mask::prompt(&run.redactor, text)
            })
        });
        match snapshot {
            Ok(snapshot) => {
                let summary = snapshot.summary();
                run.inline_result(super::engine::Status::Exited(0), &summary, "");
                run.record(summary);
                run.planning.as_mut().expect("reviewed planning flow").probe = Some(snapshot);
                self.planning_questions(id, run)
            }
            Err(error) => self.plan_without_probe_after_failure(id, run, &error),
        }
    }

    pub(super) fn plan_without_probe_after_failure(
        &self,
        id: window::Id,
        run: &mut Run,
        error: &str,
    ) -> Result<Task<Desktop>, String> {
        run.target = None;
        let task = self.planning_questions(id, run)?;
        run.notice = format!("Environment probe unavailable. {error}");
        Ok(task)
    }

    pub(super) fn validate_plan_destination(&self, run: &Run) -> Result<(), String> {
        let flow = run
            .planning
            .as_ref()
            .ok_or("The reviewed plan is unavailable.")?;
        if flow.report.is_some()
            && flow
                .destination
                .same_route(&self.planning_destination(run)?)
        {
            Ok(())
        } else {
            Err("The reviewed plan's destination changed. Open a new plan.".into())
        }
    }

    pub(super) fn planning_action(
        &self,
        id: window::Id,
        run: &mut Run,
        message: Message,
    ) -> Result<Task<Desktop>, String> {
        if run.mode != Mode::Plan {
            return Ok(Task::none());
        }
        if matches!(message, Message::Browse) && run.phase == Phase::Setup && !run.binding.private()
        {
            if let Err(reason) = self
                .terminal_agents
                .check_live_run_slot(run.binding.live()?, run.swarm)
            {
                run.notice = reason.into();
                return Ok(Task::none());
            }
            let flow = Flow::new(self.planning_destination(run)?);
            run.planning = Some(flow);
            let root = self.plan_root()?;
            let epoch = run.next(Phase::PlanStorage, "Loading saved plans…");
            return Ok(storage_task(id, epoch, move || store::list(&root)));
        }
        let Some(mut flow) = run.planning.take() else {
            return Ok(Task::none());
        };
        let task = self.planning_step(id, run, &mut flow, message);
        run.planning = Some(flow);
        task
    }

    fn planning_step(
        &self,
        id: window::Id,
        run: &mut Run,
        flow: &mut Flow,
        message: Message,
    ) -> Result<Task<Desktop>, String> {
        if let Message::Progress(epoch, progress) = message {
            self.planning_progress(run, flow, epoch, progress);
            return Ok(Task::none());
        }
        let model_completion = matches!(&message, Message::Reply(epoch, _) if run.phase == Phase::Thinking && *epoch == run.epoch);
        let completion = matches!(&message, Message::Reply(epoch, _) | Message::Storage(epoch, _) if *epoch == run.epoch);
        if run.cancel.requested() {
            if completion {
                if let Message::Reply(epoch, report) = &message
                    && model_completion
                {
                    if self.check_planning_route(run, flow).is_ok()
                        && self.agent_transport(run).is_ok()
                    {
                        run.record_model_attempts(*epoch, &report.attempts);
                    }
                    run.finish_model_repair(*epoch, true);
                }
                run.next(Phase::Cancelled, "Planning cancelled.");
            }
            return Ok(Task::none());
        }
        if let Err(error) = self.check_planning_route(run, flow) {
            if model_completion {
                run.finish_model_repair(run.epoch, false);
            }
            return Err(error);
        }
        match message {
            Message::SelectAnswer(epoch, index, option)
                if flow.can_answer(run, epoch)
                    && flow
                        .questions
                        .get(index)
                        .is_some_and(|question| option < question.options.len()) =>
            {
                flow.answer_drafts[index].selected = option;
            }
            Message::EditCustomAnswer(epoch, index, action)
                if flow.can_answer(run, epoch)
                    && flow
                        .questions
                        .get(index)
                        .is_some_and(|question| question.allow_custom_answer) =>
            {
                super::swarm::edit_bounded(
                    &mut flow.answer_drafts[index].custom,
                    action,
                    MAX_CUSTOM_ANSWER_BYTES,
                );
            }
            Message::SubmitAnswers(epoch)
                if flow.can_answer(run, epoch) && !flow.questions.is_empty() =>
            {
                let answers = flow
                    .questions
                    .iter()
                    .zip(&flow.answer_drafts)
                    .map(|(question, draft)| {
                        let mut answer =
                            mask::prompt(&run.redactor, &question.options[draft.selected])?;
                        if question.allow_custom_answer {
                            let custom = zeroize::Zeroizing::new(draft.custom.text());
                            let custom = custom.trim();
                            if !custom.is_empty() {
                                answer.push_str("\nAdditional details\n");
                                answer.push_str(&mask::prompt(&run.redactor, custom)?);
                            }
                        }
                        Ok::<_, String>(answer)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                flow.pending_answers = Some(answers);
                let review = self.prepare_plan_review(id, run, flow, Stage::Options);
                if review.is_err() {
                    flow.pending_answers = None;
                }
                return review;
            }
            Message::EditAnswers(epoch)
                if flow.can_refine(run, epoch) && !flow.questions.is_empty() =>
            {
                run.next(
                    Phase::PlanQuestions,
                    "Review or edit your planning answers.",
                );
            }
            Message::ReturnToResults(epoch) if flow.can_answer(run, epoch) => {
                if let Some(phase) = flow.previous_results() {
                    run.next(phase, "");
                }
            }
            Message::EditRefinement(epoch, action) if flow.can_refine(run, epoch) => {
                super::swarm::edit_bounded(&mut flow.refinement, action, MAX_REFINEMENT_BYTES);
            }
            Message::Revise(epoch) if flow.can_refine(run, epoch) => {
                return self.prepare_plan_review(id, run, flow, Stage::Options);
            }
            Message::Choose(epoch, choice) | Message::PrepareImplementation(epoch, choice)
                if flow.can_refine(run, epoch) && choice < flow.options.len() =>
            {
                let implement = matches!(message, Message::PrepareImplementation(..));
                if implement
                    && let Err(error) = self.current_agent_pane(run.binding.live()?, Mode::Run)
                {
                    run.notice = error.into();
                    return Ok(Task::none());
                }
                flow.chosen = Some(choice);
                let review = self.prepare_plan_review(id, run, flow, Stage::Report);
                flow.review_implementation = review.is_ok() && implement;
                return review;
            }
            Message::KeepReport(epoch)
                if run.phase == Phase::PlanImplementationReview
                    && run.epoch == epoch
                    && !run.paused =>
            {
                run.next(Phase::PlanReady, "Plan ready for later implementation.");
            }
            Message::Reply(epoch, report) if run.phase == Phase::Thinking && epoch == run.epoch => {
                let stage = self.require_plan_model_review(run).inspect_err(|_| {
                    run.finish_model_repair(epoch, false);
                })?;
                run.ai_cancel = None;
                let skill_usage = run.model_review.take().and_then(Review::into_skill_usage);
                run.record_model_response(epoch, &report);
                run.finish_model_repair(epoch, false);
                let reply = report
                    .result
                    .as_ref()
                    .map_err(Clone::clone)
                    .cloned()
                    .and_then(|mut reply| {
                        reply.redact(&|text| mask::prompt(&run.redactor, text))?;
                        Ok(reply)
                    });
                match (stage, reply) {
                    (Stage::Questions, Ok(Reply::Questions { message, questions })) => {
                        flow.answer_drafts =
                            questions.iter().map(|_| AnswerDraft::default()).collect();
                        flow.questions = questions;
                        run.next(Phase::PlanQuestions, &message);
                    }
                    (Stage::Options, Ok(Reply::Options { message, options })) => {
                        if let Some(answers) = flow.pending_answers.take() {
                            flow.answers = answers;
                        }
                        flow.options = options;
                        flow.chosen = None;
                        flow.report = None;
                        flow.saved = false;
                        run.plan = None;
                        run.next(Phase::PlanOptions, &message);
                    }
                    (Stage::Report, Ok(Reply::Report { message, report })) => {
                        let text = report.render();
                        run.record(text.clone());
                        run.plan = Some(text);
                        flow.report = Some(report);
                        flow.saved = false;
                        if std::mem::take(&mut flow.review_implementation) {
                            run.next(
                                Phase::PlanImplementationReview,
                                "Review the plan before opening its Agent run.",
                            );
                        } else {
                            run.next(Phase::PlanReady, &message);
                        }
                    }
                    (Stage::Options | Stage::Report, Err(error) | Ok(Reply::Blocked(error))) => {
                        flow.review_implementation = false;
                        let phase = if flow.pending_answers.take().is_some() {
                            Some(Phase::PlanQuestions)
                        } else {
                            flow.previous_results()
                        };
                        let Some(phase) = phase else {
                            return Err(error);
                        };
                        run.next(phase, &error);
                        return Ok(Task::none());
                    }
                    (_, Err(error) | Ok(Reply::Blocked(error))) => return Err(error),
                    _ => return Err("The reply did not match the reviewed planning stage.".into()),
                }
                if let Some(usage) = skill_usage {
                    usage.record();
                }
            }
            Message::Save
                if run.phase == Phase::PlanReady
                    && !run.binding.private()
                    && !run.paused
                    && !flow.saved =>
            {
                let root = self.plan_root()?;
                let report = flow
                    .report
                    .clone()
                    .ok_or("The reviewed plan is unavailable.")?;
                let record = Record::new(flow.destination.clone(), run.goal.clone(), report)
                    .map_err(|error| error.to_string())?;
                flow.storage_from = Phase::PlanReady;
                let epoch = run.next(Phase::PlanStorage, "Saving the reviewed plan…");
                return Ok(storage_task(id, epoch, move || {
                    store::save(&root, record, false)?;
                    Ok(Vec::new())
                }));
            }
            Message::Storage(epoch, result)
                if run.phase == Phase::PlanStorage && epoch == run.epoch =>
            {
                match (flow.storage_from, result.as_ref()) {
                    (Phase::PlanReady, Ok(_)) => {
                        flow.saved = true;
                        run.next(Phase::PlanReady, "Plan saved.");
                    }
                    (Phase::PlanReady, Err(error)) => {
                        run.next(Phase::PlanReady, error);
                    }
                    (Phase::PlanLibrary, Err(error)) => {
                        run.next(Phase::PlanLibrary, error);
                    }
                    (_, Ok(records)) => {
                        flow.records = records
                            .iter()
                            .filter(|record| record.destination.same_route(&flow.destination))
                            .cloned()
                            .collect();
                        run.next(
                            Phase::PlanLibrary,
                            "Choose a saved plan for this exact destination.",
                        );
                    }
                    (_, Err(error)) => {
                        run.next(Phase::Setup, error);
                    }
                }
            }
            Message::Open(epoch, id)
                if run.phase == Phase::PlanLibrary
                    && epoch == run.epoch
                    && !run.binding.private() =>
            {
                let record = flow
                    .records
                    .iter()
                    .find(|record| {
                        record.id == id && record.destination.same_route(&flow.destination)
                    })
                    .ok_or("The saved plan is unavailable for this route.")?;
                let mut report = record.report.clone();
                report.redact(&|text| mask::prompt(&run.redactor, text))?;
                run.goal = mask::prompt(&run.redactor, &record.goal)?;
                let text = report.render();
                run.record(text.clone());
                run.plan = Some(text);
                flow.report = Some(report);
                flow.saved = true;
                run.next(
                    Phase::PlanReady,
                    "Review the saved plan before choosing Implement.",
                );
            }
            Message::Delete(epoch, id)
                if run.phase == Phase::PlanLibrary
                    && epoch == run.epoch
                    && !run.binding.private() =>
            {
                flow.deleting = flow.records.iter().find(|record| record.id == id).cloned();
                run.next(
                    Phase::PlanLibrary,
                    "Review the selected plan before deleting it.",
                );
            }
            Message::ConfirmDelete(epoch, approved)
                if run.phase == Phase::PlanLibrary
                    && epoch == run.epoch
                    && !run.binding.private() =>
            {
                let record = flow.deleting.take();
                if let Some(record) = record.filter(|_| approved) {
                    let root = self.plan_root()?;
                    flow.storage_from = Phase::PlanLibrary;
                    let epoch = run.next(Phase::PlanStorage, "Deleting the saved plan…");
                    return Ok(storage_task(id, epoch, move || {
                        store::delete(&root, &record, false)?;
                        store::list(&root)
                    }));
                }
                run.next(Phase::PlanLibrary, "");
            }
            Message::DismissLibrary if run.phase == Phase::PlanLibrary => {
                flow.deleting = None;
                run.next(Phase::Setup, "");
            }
            _ => {}
        }
        Ok(Task::none())
    }

    fn planning_progress(&self, run: &mut Run, flow: &Flow, epoch: u64, progress: usage::Progress) {
        let ready = run.phase == Phase::Thinking
            && epoch == run.epoch
            && !run.closing
            && !run.cancel.requested()
            && run.ai_cancel.is_some()
            && self.check_planning_route(run, flow).is_ok()
            && self.require_plan_model_review(run).is_ok();
        let (permit, notice) = match progress {
            usage::Progress::Repairing(initial, permit) => (
                permit,
                (ready && run.model_repair_started(epoch, &initial))
                    .then_some("Repairing the planning response format…"),
            ),
            usage::Progress::FormatFallback(kind, permit) => (
                permit,
                (ready
                    && run.presentation.model_usage.repairing(epoch)
                        == (kind == usage::AttemptKind::Repair))
                    .then_some("Retrying without the provider's JSON response format option…"),
            ),
        };
        if let Some(notice) = notice
            && permit.try_send(()).is_ok()
        {
            run.notice = notice.into();
        }
        permit.close();
    }

    fn check_planning_route(&self, run: &Run, flow: &Flow) -> Result<(), String> {
        if flow
            .destination
            .same_route(&self.planning_destination(run)?)
        {
            Ok(())
        } else {
            Err("The source route changed. Open a new plan.".into())
        }
    }

    pub(super) fn require_plan_review_route(&self, run: &Run) -> Result<(), String> {
        let flow = run
            .planning
            .as_ref()
            .ok_or("The reviewed plan is unavailable.")?;
        self.check_planning_route(run, flow)
    }

    fn require_plan_model_review(&self, run: &Run) -> Result<Stage, String> {
        let review = run
            .model_review
            .as_ref()
            .ok_or("The reviewed planning request is unavailable.")?;
        let stage = review
            .plan_stage()
            .ok_or("The reviewed planning request is unavailable.")?;
        self.require_model_review_authority(run, review)?;
        Ok(stage)
    }

    fn prepare_plan_review(
        &self,
        id: window::Id,
        run: &mut Run,
        flow: &Flow,
        stage: Stage,
    ) -> Result<Task<Desktop>, String> {
        self.check_planning_route(run, flow)?;
        let chosen = flow
            .chosen
            .filter(|_| stage == Stage::Report)
            .and_then(|index| flow.options.get(index));
        let notes = zeroize::Zeroizing::new(flow.refinement.text());
        let notes = notes.trim();
        let refinement = (!notes.is_empty() && stage != Stage::Questions)
            .then(|| mask::prompt(&run.redactor, notes))
            .transpose()?;
        let answers = if stage == Stage::Options {
            flow.pending_answers.as_ref().unwrap_or(&flow.answers)
        } else {
            &flow.answers
        };
        let mut input = json!({
            "goal": run.goal,
            "destination": flow.destination.label(),
            "questions": flow.questions,
            "answers": answers,
            "selectedApproach": chosen,
        });
        mask_json_strings(&mut input, &|text| mask::prompt(&run.redactor, text))?;
        let mut evidence = String::new();
        if let Some(probe) = &flow.probe {
            input["probe"] = probe.input().clone();
            let _ = writeln!(evidence, "{}", probe.summary());
        }
        for (question, answer) in flow.questions.iter().zip(answers) {
            let _ = writeln!(evidence, "{}\n{answer}\n", question.question);
        }
        if let Some(refinement) = &refinement {
            input["refinement"] = json!(refinement);
            let _ = writeln!(evidence, "Planning refinement\n{refinement}");
        }
        if let Some(approach) = chosen {
            let _ = writeln!(evidence, "{}\n{}", approach.title, approach.render());
        }
        let evidence = mask::prompt(&run.redactor, &evidence)?;
        self.prepare_model_review(
            id,
            run,
            Draft::Plan {
                stage,
                input: input.to_string(),
                evidence,
            },
        )
    }

    pub(super) fn send_reviewed_plan(
        &self,
        id: window::Id,
        run: &mut Run,
        stage: Stage,
        review: &Review,
        before_dispatch: impl Fn() -> Result<(), String> + Send + 'static,
    ) -> Result<Task<Desktop>, String> {
        let (transport, limit) = review.transport();
        let (progress, updates) = async_channel::bounded(1);
        let pending = model::request_reviewed(
            transport.clone(),
            stage,
            review.system.clone(),
            review.input.clone(),
            *limit,
            Some(progress),
            before_dispatch,
        )?;
        run.transport = Some((transport.clone(), *limit));
        run.ai_cancel = Some(pending.cancel);
        let notice = if stage == Stage::Triage {
            "Checking the complexity of the request…"
        } else {
            "Waiting for the planning response…"
        };
        let epoch = run.next(Phase::Thinking, notice);
        run.begin_model_decision();
        let completion = Task::perform(
            async move {
                Arc::new(match pending.reply.recv().await {
                    Ok(Ok(report)) => report,
                    Ok(Err(error)) => Err(error).into(),
                    Err(_) => Err("The planning worker stopped.".into()).into(),
                })
            },
            move |reply| {
                let message = if stage == Stage::Triage {
                    RunMessage::Triage(super::triage::Message::Reply(epoch, reply))
                } else {
                    RunMessage::Planning(Message::Reply(epoch, reply))
                };
                Desktop::TerminalAgent(id, message)
            },
        );
        let progress = Task::run(updates, move |progress| {
            let message = if stage == Stage::Triage {
                RunMessage::Progress(epoch, progress)
            } else {
                RunMessage::Planning(Message::Progress(epoch, progress))
            };
            Desktop::TerminalAgent(id, message)
        });
        Ok(progress.chain(completion))
    }

    fn planning_destination(&self, run: &Run) -> Result<Destination, String> {
        self.agent_dispatch_policy(run)?.check()?;
        let (_, pane) = self.current_agent_pane(run.binding.live()?, Mode::Plan)?;
        let identity = if let Some(ssh) = &pane.ssh {
            let opener = pane
                .session
                .exec_opener()
                .ok_or("The authenticated SSH route closed.")?;
            let key = opener
                .verified_host_key()
                .ok_or("The authenticated SSH peer identity is unavailable.")?;
            json!({"ssh":ssh,"peerKey":key.to_openssh().map_err(|_| "Could not read the authenticated SSH peer identity.")?})
        } else {
            let local = pane
                .local_security
                .as_ref()
                .ok_or("The source local shell changed.")?;
            let mut identity = json!({"local":local,"host":kortty_pty::local_hostname(),"user":kortty_pty::local_username(),"directory":local.working_directory});
            if let Some(command) = &local.command {
                let argv = serde_json::to_string(command)
                    .map_err(|_| "Could not verify the local shell launch command.")?;
                identity["commandHash"] = json!(
                    crate::snippet_analysis::model::content_hash(&argv)
                        .map_err(|_| "Could not verify the local shell launch command.")?
                );
            }
            identity
        };
        Destination::capture(run.label.clone(), &identity.to_string())
            .map_err(|error| error.to_string())
    }

    fn plan_root(&self) -> Result<PathBuf, String> {
        self.ai_configuration()
            .and_then(|config| config.saved_root().map(std::path::Path::to_owned))
            .ok_or_else(|| "Saved plan storage is unavailable.".into())
    }
}

fn storage_task(
    id: window::Id,
    epoch: u64,
    operation: impl FnOnce() -> std::io::Result<Vec<Record>> + Send + 'static,
) -> Task<Desktop> {
    Task::perform(
        async move {
            let (send, receive) = async_channel::bounded(1);
            let worker = std::thread::Builder::new()
                .name("kortty-agent-plan-store".into())
                .spawn(move || {
                    let _ = send.send_blocking(operation().map_err(|error| error.to_string()));
                });
            Arc::new(match worker {
                Ok(_) => receive
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err("The saved plan worker stopped.".into())),
                Err(_) => Err("The saved plan worker could not start.".into()),
            })
        },
        move |result| {
            Desktop::TerminalAgent(id, RunMessage::Planning(Message::Storage(epoch, result)))
        },
    )
}
