//! Advisory planning controls and explicit saved-plan persistence.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use iced::widget::{button, column, row, text, text_input};
use iced::{Element, Task, window};

use crate::terminal_agent::planning::store::{self, Destination, Record};
use crate::terminal_agent::planning::{self as model, Approach, Question, Reply, Report, Stage};
use crate::terminal_agent::usage;

use super::request_review::{Draft, Review};
use super::{Desktop, Message as RunMessage, Mode, Phase, Run, Workspace, json, mask};

#[cfg(test)]
mod tests;

#[derive(Clone)]
pub enum Message {
    CustomAnswer(String),
    Answer(u64, String),
    Choose(u64, usize),
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

pub(super) struct Flow {
    destination: Destination,
    questions: Vec<Question>,
    answers: Vec<String>,
    custom_answer: String,
    options: Vec<Approach>,
    chosen: Option<usize>,
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
            questions: Vec::new(),
            answers: Vec::new(),
            custom_answer: String::new(),
            options: Vec::new(),
            chosen: None,
            report: None,
            records: Vec::new(),
            deleting: None,
            saved: false,
            storage_from: Phase::Setup,
        }
    }

    pub(super) fn view(&self, run: &Run) -> Option<Element<'_, RunMessage>> {
        let message = RunMessage::Planning;
        let enabled = !run.paused && !run.cancel.requested();
        match run.phase {
            Phase::PlanQuestions => {
                let question = self.questions.get(self.answers.len())?;
                let mut answers = row![].spacing(6);
                for answer in &question.options {
                    answers = answers.push(button(text(answer)).on_press_maybe(
                        enabled.then(|| message(Message::Answer(run.epoch, answer.clone()))),
                    ));
                }
                let mut controls = column![
                    text(format!(
                        "Question {} of {}",
                        self.answers.len() + 1,
                        self.questions.len()
                    ))
                    .size(13),
                    text(&question.question),
                    answers.wrap(),
                ]
                .spacing(6);
                if question.allow_custom_answer {
                    controls = controls.push(
                        row![
                            text_input("Your answer", &self.custom_answer)
                                .on_input(move |answer| message(Message::CustomAnswer(answer))),
                            button("Use answer").on_press_maybe(
                                (enabled && !self.custom_answer.trim().is_empty()).then(
                                    || message(Message::Answer(
                                        run.epoch,
                                        self.custom_answer.clone()
                                    ))
                                )
                            ),
                        ]
                        .spacing(6),
                    );
                }
                Some(controls.into())
            }
            Phase::PlanOptions => {
                let mut options = column![text("Choose an implementation approach")].spacing(8);
                for (index, option) in self.options.iter().enumerate() {
                    options = options.push(
                        column![
                            text(&option.title).size(17),
                            text(option.render()).size(13),
                            button("Draft plan for this approach").on_press_maybe(
                                enabled.then_some(message(Message::Choose(run.epoch, index))),
                            ),
                        ]
                        .spacing(4),
                    );
                }
                Some(
                    iced::widget::scrollable(options)
                        .height(iced::Length::Fixed(270.0))
                        .into(),
                )
            }
            Phase::PlanReady if self.report.is_some() && !run.binding.private() => Some(
                button(if self.saved {
                    "Plan saved"
                } else {
                    "Save plan"
                })
                .on_press_maybe((enabled && !self.saved).then_some(message(Message::Save)))
                .into(),
            ),
            Phase::PlanLibrary => {
                let mut records = column![text(format!(
                    "Saved plans for {}",
                    self.destination.label()
                ))]
                .spacing(6);
                for record in &self.records {
                    records = records.push(
                        row![
                            button(text(&record.report.title))
                                .on_press(message(Message::Open(run.epoch, record.id.clone()))),
                            button("Delete…")
                                .on_press(message(Message::Delete(run.epoch, record.id.clone()))),
                        ]
                        .spacing(6),
                    );
                }
                if self.records.is_empty() {
                    records = records.push(text("There are no saved plans for this exact route."));
                }
                if let Some(record) = &self.deleting {
                    records = records.push(
                        row![
                            text(format!("Delete ‘{}’?", record.report.title)),
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
}

impl Workspace {
    pub(super) fn planning_start(
        &self,
        id: window::Id,
        run: &mut Run,
    ) -> Result<Task<Desktop>, String> {
        let flow = Flow::new(self.planning_destination(run)?);
        let task = self.prepare_plan_review(id, run, &flow, Stage::Questions)?;
        run.planning = Some(flow);
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
            Message::CustomAnswer(answer)
                if run.phase == Phase::PlanQuestions && !run.paused && answer.len() <= 1024 =>
            {
                flow.custom_answer = answer
            }
            Message::Answer(epoch, answer)
                if run.phase == Phase::PlanQuestions && epoch == run.epoch && !run.paused =>
            {
                let question = flow
                    .questions
                    .get(flow.answers.len())
                    .ok_or("The planning question changed.")?;
                if answer.trim().is_empty()
                    || answer.len() > 1024
                    || (!question.allow_custom_answer && !question.options.contains(&answer))
                {
                    return Err("Choose an allowed planning answer.".into());
                }
                flow.answers.push(mask::prompt(&run.redactor, &answer)?);
                flow.custom_answer.clear();
                if flow.answers.len() == flow.questions.len() {
                    return self.prepare_plan_review(id, run, flow, Stage::Options);
                }
                run.next(Phase::PlanQuestions, "Answer the next planning question.");
            }
            Message::Choose(epoch, choice)
                if run.phase == Phase::PlanOptions
                    && epoch == run.epoch
                    && !run.paused
                    && choice < flow.options.len() =>
            {
                flow.chosen = Some(choice);
                return self.prepare_plan_review(id, run, flow, Stage::Report);
            }
            Message::Reply(epoch, report) if run.phase == Phase::Thinking && epoch == run.epoch => {
                let stage = self.require_plan_model_review(run).inspect_err(|_| {
                    run.finish_model_repair(epoch, false);
                })?;
                run.ai_cancel = None;
                let skill_usage = run.model_review.take().and_then(Review::into_skill_usage);
                run.record_model_attempts(epoch, &report.attempts);
                run.finish_model_repair(epoch, false);
                let mut reply = report.result.as_ref().map_err(Clone::clone)?.clone();
                reply.redact(&|text| mask::prompt(&run.redactor, text))?;
                match (stage, reply) {
                    (Stage::Questions, Reply::Questions { message, questions }) => {
                        flow.questions = questions;
                        run.next(Phase::PlanQuestions, &message);
                    }
                    (Stage::Options, Reply::Options { message, options }) => {
                        flow.options = options;
                        run.next(Phase::PlanOptions, &message);
                    }
                    (Stage::Report, Reply::Report { message, report }) => {
                        let text = report.render();
                        run.record(text.clone());
                        run.plan = Some(text);
                        flow.report = Some(report);
                        run.next(Phase::PlanReady, &message);
                    }
                    (_, Reply::Blocked(error)) => return Err(error),
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
        let chosen = flow.chosen.and_then(|index| flow.options.get(index));
        let input = json!({"goal":run.goal,"destination":flow.destination.label(),"questions":flow.questions,"answers":flow.answers,"selectedApproach":chosen}).to_string();
        let input = mask::prompt(&run.redactor, &input)?;
        let mut evidence = String::new();
        for (question, answer) in flow.questions.iter().zip(&flow.answers) {
            let _ = writeln!(evidence, "{}\n{answer}\n", question.question);
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
                input,
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
        let epoch = run.next(Phase::Thinking, "Waiting for the planning response…");
        let completion = Task::perform(
            async move {
                Arc::new(match pending.reply.recv().await {
                    Ok(Ok(report)) => report,
                    Ok(Err(error)) => Err(error).into(),
                    Err(_) => Err("The planning worker stopped.".into()).into(),
                })
            },
            move |reply| {
                Desktop::TerminalAgent(id, RunMessage::Planning(Message::Reply(epoch, reply)))
            },
        );
        let progress = Task::run(updates, move |progress| {
            Desktop::TerminalAgent(id, RunMessage::Planning(Message::Progress(epoch, progress)))
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
            json!({"local":local,"host":kortty_pty::local_hostname(),"user":kortty_pty::local_username(),"directory":local.working_directory})
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
