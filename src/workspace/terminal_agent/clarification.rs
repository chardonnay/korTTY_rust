//! Interactive answers to a Run model's clarification question.

use std::time::{Duration, Instant};

use iced::widget::{button, column, row, text, text_input};
use iced::{Element, Task, window};
use serde::Serialize;

use super::{Desktop, Message as RunMessage, Phase, Run, Workspace, mask, request_review, triage};

pub(super) const CHOICE_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_ANSWER_BYTES: usize = 1024;

#[cfg(test)]
mod tests;

#[derive(Clone)]
pub enum Message {
    Draft(u64, String),
    Instructions(u64, String),
    Suggested(u64, usize),
    Submit(u64),
    Defer(u64),
    Expired(u64),
}

impl Message {
    fn epoch(&self) -> u64 {
        match *self {
            Self::Draft(epoch, _)
            | Self::Instructions(epoch, _)
            | Self::Suggested(epoch, _)
            | Self::Submit(epoch)
            | Self::Defer(epoch)
            | Self::Expired(epoch) => epoch,
        }
    }
}

pub(super) struct Question {
    question: String,
    suggestions: Vec<String>,
    explanation: String,
    draft: String,
    instructions: String,
    deadline: Instant,
}

#[derive(Serialize)]
pub(super) struct Answer {
    question: String,
    answer: Option<String>,
    instructions: Option<String>,
}

impl Answer {
    pub(super) fn unanswered(question: String) -> Self {
        Self {
            question,
            answer: None,
            instructions: None,
        }
    }

    pub(super) fn user_text(&self) -> impl Iterator<Item = &str> {
        self.answer
            .as_deref()
            .into_iter()
            .chain(self.instructions.as_deref())
    }
}

impl Question {
    pub(super) fn new(question: String, suggestions: Vec<String>, explanation: String) -> Self {
        Self {
            question,
            suggestions,
            explanation,
            draft: String::new(),
            instructions: String::new(),
            deadline: Instant::now() + CHOICE_TIMEOUT,
        }
    }

    pub(super) fn view<'a>(&'a self, run: &Run) -> Element<'a, Message> {
        let active = !run.paused && !run.closing && !run.cancel.requested();
        let epoch = run.epoch;
        let mut suggestions = row![].spacing(6);
        for (index, answer) in self.suggestions.iter().enumerate() {
            suggestions = suggestions.push(
                button(text(answer).wrapping(text::Wrapping::WordOrGlyph))
                    .on_press_maybe(active.then_some(Message::Suggested(epoch, index))),
            );
        }
        let submit = (active && !self.draft.trim().is_empty()).then_some(Message::Submit(epoch));
        column![
            text(&self.question)
                .size(16)
                .wrapping(text::Wrapping::WordOrGlyph),
            (!self.explanation.trim().is_empty()
                && self.explanation.trim() != self.question.trim())
            .then(|| text(&self.explanation).wrapping(text::Wrapping::WordOrGlyph)),
            text_input("Additional instructions (optional)", &self.instructions)
                .on_input_maybe(active.then_some(move |value| Message::Instructions(epoch, value))),
            suggestions.wrap(),
            text_input("Write your own answer", &self.draft)
                .on_input_maybe(active.then_some(move |value| Message::Draft(epoch, value)))
                .on_submit_maybe(submit.clone()),
            row![
                button("Use my answer").on_press_maybe(submit),
                button("Let the agent decide")
                    .on_press_maybe(active.then_some(Message::Defer(epoch))),
            ]
            .spacing(6)
            .wrap(),
        ]
        .spacing(6)
        .into()
    }
}

impl Run {
    pub(super) fn clarification_input(
        &self,
        mask: impl Fn(&str) -> Result<String, String>,
    ) -> Result<Vec<Answer>, String> {
        self.clarification_answers
            .iter()
            .map(|entry| {
                Ok(Answer {
                    question: mask(&entry.question)?,
                    answer: entry.answer.as_deref().map(&mask).transpose()?,
                    instructions: entry.instructions.as_deref().map(&mask).transpose()?,
                })
            })
            .collect()
    }

    pub(super) fn has_timed_input(&self) -> bool {
        matches!(self.phase, Phase::Question | Phase::TriageChoice)
            && !self.closing
            && !self.cancel.requested()
    }

    fn expired_input(&self, now: Instant) -> Option<RunMessage> {
        if !self.has_timed_input() {
            return None;
        }
        match self.phase {
            Phase::Question
                if self
                    .question
                    .as_ref()
                    .is_some_and(|question| now >= question.deadline) =>
            {
                Some(RunMessage::Clarification(Message::Expired(self.epoch)))
            }
            Phase::TriageChoice
                if self
                    .triage
                    .choice
                    .as_ref()
                    .is_some_and(|choice| choice.expired(now)) =>
            {
                Some(RunMessage::Triage(triage::Message::Expired(self.epoch)))
            }
            _ => None,
        }
    }
}

impl Workspace {
    pub(super) fn clarify_agent(
        &self,
        id: window::Id,
        run: &mut Run,
        message: Message,
    ) -> Result<Task<Desktop>, String> {
        if run.phase != Phase::Question || run.epoch != message.epoch() || !run.has_timed_input() {
            return Ok(Task::none());
        }
        let Some(question) = &mut run.question else {
            return Ok(Task::none());
        };
        let expired = Instant::now() >= question.deadline;
        let (answer, instructions) = if expired {
            (None, None)
        } else {
            if run.paused {
                return Ok(Task::none());
            }
            match message {
                Message::Draft(_, value) if value.len() <= MAX_ANSWER_BYTES => {
                    question.draft = value;
                    return Ok(Task::none());
                }
                Message::Instructions(_, value) if value.len() <= MAX_ANSWER_BYTES => {
                    question.instructions = value;
                    return Ok(Task::none());
                }
                Message::Suggested(_, index) => {
                    let Some(answer) = question.suggestions.get(index) else {
                        return Ok(Task::none());
                    };
                    (
                        Some(answer.trim().to_owned()),
                        nonempty(&question.instructions),
                    )
                }
                Message::Submit(_) if !question.draft.trim().is_empty() => {
                    (nonempty(&question.draft), nonempty(&question.instructions))
                }
                Message::Defer(_) => (None, nonempty(&question.instructions)),
                _ => return Ok(Task::none()),
            }
        };
        let question = run.question.take().expect("active clarification").question;
        let outbound = run
            .profile()
            .is_some_and(super::ProfileChoice::is_reviewed_outbound);
        let credential = if outbound {
            self.agent_transport(run)?.0.completion_secret()
        } else {
            None
        };
        let redact = |value: &str| -> Result<String, String> {
            let masked = mask::prompt(&run.redactor, value)?;
            if outbound {
                request_review::mask_https_text(
                    &masked,
                    credential.as_deref().map(String::as_str),
                    self.vault.as_ref(),
                )
            } else {
                Ok(masked)
            }
        };
        let answer = answer.as_deref().map(redact).transpose()?;
        let instructions = instructions.as_deref().map(redact).transpose()?;
        let activity = if let Some(answer) = &answer {
            format!("Your answer\n{answer}")
        } else if expired {
            "The clarification expired. Review the next request.".into()
        } else {
            "You asked the agent to decide. Review the next request.".into()
        };
        run.clarification_answers.push(Answer {
            question,
            answer,
            instructions,
        });
        run.record(activity);
        run.turns = run.turns.saturating_sub(1);
        self.agent_turn(id, run)
    }

    pub(in crate::workspace) fn expire_agent_inputs(&self) -> Task<Desktop> {
        let now = Instant::now();
        Task::batch(self.terminal_agents.runs.iter().filter_map(|(&id, run)| {
            run.expired_input(now)
                .map(|message| Task::done(Desktop::TerminalAgent(id, message)))
        }))
    }
}

fn nonempty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}
