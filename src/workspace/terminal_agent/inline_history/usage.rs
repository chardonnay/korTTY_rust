//! Projection of already accepted model-attempt metadata; never adds model input or authority.

use crate::terminal_agent::usage::{Attempt, AttemptKind, AttemptOutcome, Report};

use super::*;

impl Run {
    pub(in crate::workspace::terminal_agent) fn begin_model_decision(&mut self) {
        let title = self.initial_model_title();
        if let Some(history) = &mut self.inline {
            history.model_step(
                self.epoch,
                AttemptKind::Initial,
                Step {
                    kind: Kind::Thinking,
                    title: title.into(),
                    state: StepState::Running,
                    ..Step::default()
                },
            );
        }
    }

    pub(in crate::workspace::terminal_agent) fn finish_model_decision_without_report(
        &mut self,
        state: StepState,
    ) {
        if let Some(step) = self
            .inline
            .as_mut()
            .and_then(|history| history.model_step_mut(self.epoch, AttemptKind::Initial))
            .filter(|step| step.state == StepState::Running)
        {
            step.elapsed_ms = step.elapsed_at(Instant::now());
            step.state = state;
            step.detail = "No model attempt metadata is available.".into();
        }
    }

    pub(in crate::workspace::terminal_agent) fn record_model_response<T>(
        &mut self,
        epoch: u64,
        report: &Report<T>,
    ) {
        if epoch != self.epoch {
            return;
        }
        self.record_sent_context(report);
        self.record_model_attempts(epoch, &report.attempts);
        self.finish_model_decision_without_report(if report.result.is_ok() {
            StepState::Completed
        } else {
            StepState::Failed
        });
    }

    fn initial_model_title(&self) -> &'static str {
        if self.phase == Phase::FinalAdvice {
            "Final AI response"
        } else {
            "AI decision"
        }
    }

    /// The caller has validated the current run, source, policy and request epoch.
    pub(in crate::workspace::terminal_agent) fn model_repair_started(
        &mut self,
        epoch: u64,
        initial: &Attempt,
    ) -> bool {
        let format_failure = initial.outcome == AttemptOutcome::InvalidReply;
        let final_limit = self.phase == Phase::FinalAdvice
            && matches!(
                initial.outcome,
                AttemptOutcome::TokenLimit | AttemptOutcome::LikelyTokenLimit
            );
        if epoch != self.epoch
            || initial.kind != AttemptKind::Initial
            || !(format_failure || final_limit)
        {
            return false;
        }
        self.record_model_attempts(epoch, std::slice::from_ref(initial));
        if !self.presentation.model_usage.start_repair(epoch) {
            return false;
        }
        // The initial call finished; its schema failure starts a separate repair attempt.
        if let Some(step) = self
            .inline
            .as_mut()
            .and_then(|history| history.model_step_mut(epoch, AttemptKind::Initial))
        {
            if format_failure {
                step.state = StepState::Completed;
            }
            step.detail.push_str("\nThe AI response needed repair.");
        }
        self.project_repair_state(epoch, StepState::Running);
        true
    }

    /// Close an interrupted display row without manufacturing an attempt or token counts.
    pub(in crate::workspace::terminal_agent) fn finish_model_repair(
        &mut self,
        epoch: u64,
        cancelled: bool,
    ) {
        if self
            .presentation
            .model_usage
            .finish_repair(epoch, cancelled)
        {
            self.project_repair_state(
                epoch,
                if cancelled {
                    StepState::Cancelled
                } else {
                    StepState::Failed
                },
            );
        }
    }

    fn project_repair_state(&mut self, epoch: u64, state: StepState) {
        let detail = self
            .presentation
            .model_usage
            .row(epoch, AttemptKind::Repair)
            .unwrap_or_default();
        if let Some(history) = &mut self.inline {
            history.model_step(
                epoch,
                AttemptKind::Repair,
                Step {
                    kind: Kind::Thinking,
                    title: "AI response repair".into(),
                    detail,
                    state,
                    ..Step::default()
                },
            );
        }
    }

    pub(in crate::workspace::terminal_agent) fn record_model_attempts(
        &mut self,
        epoch: u64,
        attempts: &[Attempt],
    ) {
        let initial_title = self.initial_model_title();
        for attempt in attempts {
            if !self.presentation.model_usage.accept(epoch, attempt) {
                continue;
            }
            let detail = self
                .presentation
                .model_usage
                .row(epoch, attempt.kind)
                .unwrap_or_default();
            if let Some(history) = &mut self.inline {
                history.model_step(
                    epoch,
                    attempt.kind,
                    Step {
                        kind: Kind::Thinking,
                        title: match attempt.kind {
                            AttemptKind::Initial => initial_title,
                            AttemptKind::Repair => "AI response repair",
                        }
                        .into(),
                        detail,
                        elapsed_ms: Some(
                            attempt.elapsed.as_millis().min(u128::from(u64::MAX)) as u64
                        ),
                        tokens: attempt.total_tokens,
                        state: match attempt.outcome {
                            AttemptOutcome::Completed => StepState::Completed,
                            AttemptOutcome::Cancelled => StepState::Cancelled,
                            AttemptOutcome::InvalidReply
                            | AttemptOutcome::TokenLimit
                            | AttemptOutcome::LikelyTokenLimit
                            | AttemptOutcome::Failed => StepState::Failed,
                        },
                        ..Step::default()
                    },
                );
            }
        }
    }
}

impl History {
    fn model_step_mut(&mut self, epoch: u64, kind: AttemptKind) -> Option<&mut Step> {
        let (current, ids) = self.model_steps?;
        let id = ids[attempt_index(kind)].filter(|_| current == epoch)?;
        self.transcript.steps.iter_mut().find(|step| step.id == id)
    }

    fn model_step(&mut self, epoch: u64, kind: AttemptKind, mut step: Step) {
        if self
            .model_steps
            .as_ref()
            .is_some_and(|(current, _)| *current > epoch)
        {
            return;
        }
        if self
            .model_steps
            .as_ref()
            .is_none_or(|(current, _)| *current != epoch)
        {
            self.model_steps = Some((epoch, [None; 2]));
        }
        let index = attempt_index(kind);
        let (_, ids) = self
            .model_steps
            .as_mut()
            .expect("model step epoch is initialized");
        step.id = *ids[index].get_or_insert_with(|| {
            let id = self.next_step;
            self.next_step += 1;
            id
        });
        if let Some(existing) = self
            .transcript
            .steps
            .iter_mut()
            .find(|existing| existing.id == step.id)
        {
            step.recorded_at = existing.recorded_at;
            step.started_at = existing.started_at;
            if step.state != StepState::Running && step.elapsed_ms.is_none() {
                step.elapsed_ms = existing.elapsed_at(Instant::now());
            }
            *existing = step;
        } else {
            self.step(step);
        }
    }
}

fn attempt_index(kind: AttemptKind) -> usize {
    match kind {
        AttemptKind::Initial => 0,
        AttemptKind::Repair => 1,
    }
}

#[cfg(test)]
mod tests;
