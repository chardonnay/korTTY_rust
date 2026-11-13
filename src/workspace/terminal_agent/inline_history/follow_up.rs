//! Bounded evidence from a settled run, independent of the selected history presenter.

use crate::terminal_logging::Redactor;
use crate::workspace::terminal_ai::agent_follow_up::{Step as ChatStep, StepKind, StepStatus};

use super::{FollowUpTrace, Kind, Phase, Run, Step, StepState, mask};

// Leave room for quoting, labels and the separate question in the Chat review.
const MAX_TRACE_TEXT_BYTES: usize = 10 * 1024;
const MAX_GOAL_BYTES: usize = 2 * 1024;
const MAX_ANSWER_BYTES: usize = 3 * 1024;
const MAX_TITLE_BYTES: usize = 240;
const MAX_DETAIL_BYTES: usize = 768;
const MAX_COMMAND_BYTES: usize = 1024;
const MAX_OUTPUT_LINES: usize = 4;
const MAX_OUTPUT_LINE_BYTES: usize = 256;

impl Run {
    pub(in crate::workspace::terminal_agent) fn follow_up_trace(
        &self,
    ) -> Result<FollowUpTrace, String> {
        self.binding.live()?;
        let history = self
            .inline
            .as_ref()
            .filter(|_| !self.swarm && self.profile().is_some())
            .ok_or("This run has no activity to continue in Chat.")?;
        let (status, answer) = match self.phase {
            Phase::PlanReady => (
                "The agent plan is ready for review.",
                self.plan.as_deref().unwrap_or_default(),
            ),
            Phase::Finished => (
                "Agent run finished.",
                history.transcript.final_answer.as_str(),
            ),
            Phase::Failed => (
                "Agent run failed.",
                history.transcript.final_answer.as_str(),
            ),
            Phase::Blocked => (
                "Agent run was blocked.",
                history.transcript.final_answer.as_str(),
            ),
            Phase::Cancelled => (
                "Agent run was cancelled. Approved commands may have had effects.",
                history.transcript.final_answer.as_str(),
            ),
            _ => return Err("Finish or cancel this run before continuing in Chat.".into()),
        };
        let goal = mask::prompt(&self.redactor, &self.goal)?;
        if goal.trim().is_empty() {
            return Err("This run has no goal to continue in Chat.".into());
        }
        let answer = if answer.trim().is_empty() {
            self.chat_handoff_notice.as_deref().unwrap_or_default()
        } else {
            answer
        };
        let answer = mask::prompt(&self.redactor, answer)?;
        let answer = if answer.trim().is_empty() {
            status
        } else {
            &answer
        };
        let goal = clipped(&goal, MAX_GOAL_BYTES);
        let answer = clipped(answer, MAX_ANSWER_BYTES);
        let mut remaining = MAX_TRACE_TEXT_BYTES - goal.len() - answer.len();
        let mut steps = Vec::new();
        for step in history.transcript.steps.iter().rev() {
            let Some(step) = chat_step(step, &self.redactor)? else {
                continue;
            };
            let bytes = step.title.len()
                + step.detail.len()
                + step.command.as_ref().map_or(0, String::len)
                + step.output.iter().map(String::len).sum::<usize>();
            if bytes > remaining {
                break;
            }
            remaining -= bytes;
            steps.push(step);
        }
        steps.reverse();
        Ok(FollowUpTrace {
            goal,
            answer,
            steps,
        })
    }
}

fn chat_step(step: &Step, redactor: &Redactor) -> Result<Option<ChatStep>, String> {
    let (kind, fallback) = match step.kind {
        Kind::Thinking => return Ok(None),
        Kind::Action => return command_step(step, redactor).map(Some),
        Kind::Question => (StepKind::Question, "Agent question"),
        Kind::Error => (StepKind::Warning, "Agent warning"),
        Kind::Message => (StepKind::Decision, "Agent decision"),
    };
    let title = clipped(&mask::prompt(redactor, &step.title)?, MAX_TITLE_BYTES);
    Ok(Some(ChatStep {
        kind,
        title: if title.trim().is_empty() {
            fallback.into()
        } else {
            title
        },
        detail: clipped(&mask::prompt(redactor, &step.detail)?, MAX_DETAIL_BYTES),
        command: None,
        output: Vec::new(),
        exit_code: None,
        status: None,
        elapsed_seconds: None,
        truncated: None,
    }))
}

fn command_step(step: &Step, redactor: &Redactor) -> Result<ChatStep, String> {
    let status = match step.state {
        StepState::Completed => StepStatus::Completed,
        StepState::Failed => StepStatus::Failed,
        StepState::Cancelled => StepStatus::Cancelled,
        StepState::Running => {
            return Ok(ChatStep {
                kind: StepKind::Warning,
                title: "Command completion was not confirmed".into(),
                detail: String::new(),
                command: None,
                output: Vec::new(),
                exit_code: None,
                status: None,
                elapsed_seconds: None,
                truncated: None,
            });
        }
    };
    let start = step.output.len().saturating_sub(MAX_OUTPUT_LINES);
    let mut truncated = start != 0;
    let output = step.output[start..]
        .iter()
        .map(|line| {
            let line = mask::prompt(redactor, line)?;
            truncated |= line.len() > MAX_OUTPUT_LINE_BYTES;
            Ok(clipped(&line, MAX_OUTPUT_LINE_BYTES))
        })
        .collect::<Result<_, String>>()?;
    let command = step
        .command
        .as_deref()
        .map(|command| mask::prompt(redactor, command))
        .transpose()?;
    Ok(ChatStep {
        kind: StepKind::Command,
        title: "Approved command".into(),
        detail: String::new(),
        command: command
            .as_deref()
            .map(|command| clipped(command, MAX_COMMAND_BYTES)),
        output,
        exit_code: step.exit_code.map(i64::from),
        status: Some(status),
        elapsed_seconds: Some(step.elapsed_ms / 1000),
        truncated: truncated.then_some(true),
    })
}

fn clipped(text: &str, maximum: usize) -> String {
    if text.len() <= maximum {
        return text.into();
    }
    let end = text.floor_char_boundary(maximum - '…'.len_utf8());
    format!("{}…", &text[..end])
}

#[cfg(test)]
mod tests;
