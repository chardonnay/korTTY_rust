//! Reviewed complexity assessment for the initial interactive Run request.

use std::sync::Arc;
use std::time::Instant;

use iced::widget::{button, column, row, scrollable, text, text_input};
use iced::{Element, Task, window};

use crate::terminal_agent::planning::{Approach, Reply, TriageMode};
use crate::terminal_agent::usage;

use super::{
    Desktop, Message as RunMessage, Mode, Phase, Run, Workspace, clarification::CHOICE_TIMEOUT,
    mask,
};

const MAX_INSTRUCTIONS: usize = 2048;

#[cfg(test)]
mod tests;

#[derive(Clone)]
pub enum Message {
    Reply(u64, Arc<usage::Report<Reply>>),
    Idea(u64, String),
    Instructions(u64, String),
    Choose(u64, Selection),
    Expired(u64),
}

#[derive(Clone, Copy)]
pub enum Selection {
    Option(usize),
    OwnIdea,
    AsRequested,
}

#[derive(Default)]
pub(super) struct State {
    pub(super) checked: bool,
    pub(super) choice: Option<Choice>,
}

pub(super) struct Choice {
    reasons: Vec<String>,
    recommended: usize,
    options: Vec<Approach>,
    idea: String,
    instructions: String,
    deadline: Instant,
}

impl Choice {
    pub(super) fn expired(&self, now: Instant) -> bool {
        now >= self.deadline
    }

    pub(super) fn view<'a>(&'a self, run: &'a Run) -> Element<'a, RunMessage> {
        let active = !run.paused && !run.closing && !run.cancel.requested();
        let epoch = run.epoch;
        let select = |choice| RunMessage::Triage(Message::Choose(epoch, choice));
        let mut proposals =
            column![text(self.reasons.join("\n")).wrapping(text::Wrapping::WordOrGlyph)].spacing(8);
        for (index, option) in self.options.iter().enumerate() {
            proposals = proposals.push(
                column![
                    text(&option.title).size(16),
                    text(option.render()).wrapping(text::Wrapping::WordOrGlyph),
                    button(if index == self.recommended {
                        "Use recommended approach"
                    } else {
                        "Use this approach"
                    })
                    .on_press_maybe(active.then_some(select(Selection::Option(index)))),
                ]
                .spacing(5),
            );
        }
        column![
            text("Choose an approach").size(17),
            scrollable(proposals).height(200),
            text_input(
                "Additional instructions for the selected approach (optional)",
                &self.instructions
            )
            .on_input_maybe(
                active.then_some(move |value| RunMessage::Triage(Message::Instructions(
                    epoch, value
                )))
            ),
            text_input("Describe your own approach", &self.idea).on_input_maybe(
                active.then_some(move |value| RunMessage::Triage(Message::Idea(epoch, value)))
            ),
            row![
                button("Use my approach").on_press_maybe(
                    (active && !self.idea.trim().is_empty()).then_some(select(Selection::OwnIdea))
                ),
                button("Continue as requested")
                    .on_press_maybe(active.then_some(select(Selection::AsRequested))),
            ]
            .spacing(5)
            .wrap(),
        ]
        .spacing(6)
        .into()
    }

    fn plan(&self, selection: Selection) -> Option<String> {
        let mut plan = match selection {
            Selection::Option(index) => {
                let option = self.options.get(index)?;
                format!("{}\n\n{}", option.title, option.render())
            }
            Selection::OwnIdea if !self.idea.trim().is_empty() => self.idea.trim().to_owned(),
            Selection::OwnIdea => return None,
            Selection::AsRequested => String::new(),
        };
        if !self.instructions.trim().is_empty() {
            if !plan.is_empty() {
                plan.push_str("\n\n");
            }
            plan.push_str("Additional instructions from the user:\n");
            plan.push_str(self.instructions.trim());
        }
        Some(plan)
    }
}

impl Workspace {
    pub(super) fn triage_mode(&self, run: &Run) -> Option<TriageMode> {
        if run.mode != Mode::Run
            || run.swarm
            || run.profile().is_none()
            || run.binding.live().is_err()
            || !run.probe_reviewed
            || run.plan.is_some()
            || run.turns != 0
            || !self.clipboard.policy.ai_planning_allowed()
        {
            return None;
        }
        let mode = self
            .ai_configuration()?
            .agent_triage(&self.clipboard.policy);
        (mode != TriageMode::Never).then_some(mode)
    }

    pub(super) fn triage_action(
        &self,
        id: window::Id,
        run: &mut Run,
        message: Message,
    ) -> Result<Task<Desktop>, String> {
        if run.closing {
            return Ok(Task::none());
        }
        let epoch = run.epoch;
        match message {
            Message::Reply(revision, report)
                if revision == epoch && run.phase == Phase::Thinking =>
            {
                if run.cancel.requested() {
                    run.finish_model_repair(epoch, true);
                    run.next(Phase::Cancelled, "Complexity check cancelled.");
                    return Ok(Task::none());
                }
                let Some(review) = &run.model_review else {
                    return Ok(Task::none());
                };
                let Some(mode) = review.triage_mode() else {
                    return Ok(Task::none());
                };
                self.require_model_review_authority(run, review)
                    .inspect_err(|_| run.finish_model_repair(epoch, false))?;
                run.ai_cancel = None;
                run.record_model_response(epoch, &report);
                run.finish_model_repair(epoch, false);
                let mut reply = report.result.clone();
                if let Ok(reply) = &mut reply
                    && reply
                        .redact(&|value| mask::prompt(&run.redactor, value))
                        .is_err()
                {
                    return self.skip_triage(
                        id,
                        run,
                        "The complexity check returned no usable result.",
                    );
                }
                let Ok(Reply::Triage {
                    complex,
                    reasons,
                    recommended,
                    options,
                }) = reply
                else {
                    return self.skip_triage(
                        id,
                        run,
                        "The complexity check returned no usable result.",
                    );
                };
                if !complex && mode == TriageMode::Auto {
                    return self.skip_triage(
                        id,
                        run,
                        "The complexity check found a straightforward request.",
                    );
                }
                run.triage.choice = Some(Choice {
                    reasons,
                    recommended,
                    options,
                    idea: String::new(),
                    instructions: String::new(),
                    deadline: Instant::now() + CHOICE_TIMEOUT,
                });
                run.next(
                    Phase::TriageChoice,
                    "Choose an approach or continue with the original request.",
                );
            }
            message if run.phase == Phase::TriageChoice && !run.cancel.requested() => {
                let Some(choice) = &mut run.triage.choice else {
                    return Ok(Task::none());
                };
                let editing_idea = matches!(&message, Message::Idea(..));
                match message {
                    Message::Idea(revision, value) | Message::Instructions(revision, value)
                        if revision == epoch && !run.paused && value.len() <= MAX_INSTRUCTIONS =>
                    {
                        if editing_idea {
                            choice.idea = value;
                        } else {
                            choice.instructions = value;
                        }
                    }
                    Message::Choose(revision, selection) if revision == epoch && !run.paused => {
                        if choice.expired(Instant::now()) {
                            return self.skip_triage(id, run, "The approach choice expired.");
                        }
                        let Some(plan) = choice.plan(selection) else {
                            return Ok(Task::none());
                        };
                        let review = run
                            .model_review
                            .as_ref()
                            .ok_or("The reviewed assessment is unavailable.")?;
                        self.require_model_review_authority(run, review)?;
                        if !plan.is_empty() {
                            run.plan = Some(mask::prompt(&run.redactor, &plan)?);
                        }
                        return self.skip_triage(id, run, "Approach selection completed.");
                    }
                    Message::Expired(revision)
                        if revision == epoch && choice.expired(Instant::now()) =>
                    {
                        return self.skip_triage(id, run, "The approach choice expired.");
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        Ok(Task::none())
    }

    fn skip_triage(
        &self,
        id: window::Id,
        run: &mut Run,
        notice: &str,
    ) -> Result<Task<Desktop>, String> {
        run.triage.choice = None;
        if let Some(usage) = run
            .model_review
            .take()
            .and_then(super::request_review::Review::into_skill_usage)
        {
            usage.record();
        }
        run.record(notice.into());
        self.agent_turn(id, run)
    }
}
