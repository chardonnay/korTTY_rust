use std::sync::atomic::Ordering;
use std::time::Duration;

use iced::widget::{button, column, progress_bar, row, scrollable, text};
use iced::{Element, Length, Size, Task, window};

use crate::workspace::{coding_agents::duration_text, dialog_placement::Kind};

use super::{
    Active, Desktop, Event, JournalMessage, PROGRESS_TITLE, Receipt, State, Workspace,
    summary_live_status,
};

pub(super) struct Details {
    pub(super) id: window::Id,
    batch: String,
    status: Option<String>,
}

impl Details {
    pub(super) fn matches(&self, id: window::Id, batch: &str) -> bool {
        self.id == id && self.batch == batch
    }
}

impl State {
    pub(super) fn current_receipt(&self) -> Option<&Receipt> {
        self.active
            .as_ref()
            .filter(|active| !active.stop.load(Ordering::Acquire))
            .map(|active| &active.receipt)
            .or_else(|| {
                self.queue
                    .pending()
                    .iter()
                    .find(|receipt| self.paused.contains(&receipt.generation))
                    .or_else(|| self.queue.pending().front())
            })
    }

    pub(super) fn update_details(&mut self, batch: &str) {
        if let Some(details) = self
            .details
            .as_mut()
            .filter(|details| details.batch == batch)
        {
            details.status.clone_from(&self.status);
        }
    }

    fn active_details(&self, details: &Details) -> Option<&Active> {
        self.active.as_ref().filter(|active| {
            active.receipt.batch == details.batch && !active.stop.load(Ordering::Acquire)
        })
    }
}

impl Workspace {
    pub(super) fn show_current_journal_summary_progress(
        &mut self,
        manager: window::Id,
    ) -> Task<Desktop> {
        if self.visible_journal_manager_owner(manager).is_none() || self.journal_edit_busy(manager)
        {
            return Task::none();
        }
        let Some(receipt) = self.journal_summaries.current_receipt() else {
            return Task::none();
        };
        self.open_journal_summary_progress(receipt.batch.clone())
    }

    pub(super) fn open_journal_summary_progress(&mut self, batch: String) -> Task<Desktop> {
        if let Some(details) = &mut self.journal_summaries.details {
            if details.batch != batch {
                details.batch = batch;
                details.status = None;
            }
            return window::minimize(details.id, false).chain(window::gain_focus(details.id));
        }
        let (id, task) = self.open_placed_dialog(
            Kind::JournalSummaryProgress,
            window::Settings {
                size: Size::new(520.0, 280.0),
                min_size: Some(Size::new(400.0, 240.0)),
                position: window::Position::Centered,
                exit_on_close_request: false,
                ..window::Settings::default()
            },
        );
        self.journal_summaries.details = Some(Details {
            id,
            batch,
            status: None,
        });
        task.map(|_| Desktop::Noop)
    }

    pub(in crate::workspace) fn is_journal_summary_progress(&self, id: window::Id) -> bool {
        self.journal_summaries
            .details
            .as_ref()
            .is_some_and(|details| details.id == id)
    }

    pub(in crate::workspace) fn journal_summary_progress_open(&self) -> bool {
        self.journal_summaries.details.is_some()
    }

    pub(in crate::workspace) fn journal_summary_progress_closed(&mut self, id: window::Id) {
        if self.is_journal_summary_progress(id) {
            self.journal_summaries.details = None;
        }
    }

    pub(in crate::workspace) fn close_journal_summary_progress(
        &mut self,
        id: window::Id,
    ) -> Option<Task<Desktop>> {
        if !self.is_journal_summary_progress(id) {
            return None;
        }
        self.journal_summary_progress_closed(id);
        let close = self.close_placed_dialog(id);
        Some(if self.should_exit_after_window_close() {
            Task::batch([close, self.quit()])
        } else {
            close
        })
    }

    pub(in crate::workspace) fn journal_summary_progress_view(
        &self,
        id: window::Id,
    ) -> Option<Element<'_, Desktop>> {
        let state = &self.journal_summaries;
        let details = state.details.as_ref().filter(|details| details.id == id)?;
        let pending = || {
            state
                .queue
                .pending()
                .iter()
                .filter(|receipt| receipt.batch == details.batch)
        };
        let send = move |event| Desktop::SessionJournal(id, JournalMessage::Summary(event));
        let active = state.active_details(details);
        let waiting = pending().next();
        let mut content = column![].spacing(10);
        if let Some(active) = active {
            content = content.push(text(&active.receipt.title).size(16));
            let outcome = active.visible.outcome;
            let elapsed = active.started.elapsed();
            content = content.push(text(match outcome.planned_windows {
                Some(total) => format!("{} of {total} summaries written", outcome.windows),
                None => format!("{} summaries written", outcome.windows),
            }));
            if let Some(total) = outcome.planned_windows.filter(|total| *total > 0) {
                content = content.push(progress_bar(
                    0.0..=total as f32,
                    outcome.windows.min(total) as f32,
                ));
                if let Some(eta) = estimate(elapsed, outcome.windows, total) {
                    content = content.push(text(format!(
                        "Estimated time remaining for summaries · {}",
                        duration_text(eta.as_secs())
                    )));
                }
            }
            content = content.push(text(format!(
                "Elapsed · {}",
                duration_text(elapsed.as_secs())
            )));
            if let Some(preview) = &active.visible.preview {
                content = content.push(text(summary_live_status(preview)));
            } else if outcome.planned_windows.is_none() && outcome.windows == 0 {
                content = content.push(text("Reading the journal capture"));
            }
        } else if let Some(receipt) = waiting {
            content = content.push(text(&receipt.title).size(16));
            content = content.push(text(if state.paused.contains(&receipt.generation) {
                "Summary pass paused"
            } else {
                "Waiting to start the summary pass"
            }));
        }
        let pending_count = pending().count();
        if pending_count > 1 {
            content = content.push(text(format!(
                "{pending_count} journals remain in this pass"
            )));
        }
        if let Some(status) = details.status.as_deref().filter(|_| active.is_none()) {
            content = content.push(text(status));
        } else if active.is_none() && waiting.is_none() {
            content = content.push(text("The summary pass has ended."));
        }
        let controls = row![
            button("Close").on_press(send(Event::CloseProgress)),
            button("Cancel pass").on_press_maybe(
                (active.is_some() || waiting.is_some())
                    .then(|| send(Event::CancelProgress(details.batch.clone())))
            ),
        ]
        .spacing(8);
        Some(
            column![
                text(PROGRESS_TITLE).size(21),
                scrollable(content.width(Length::Fill)).height(Length::Fill),
                controls,
            ]
            .padding(18)
            .spacing(10)
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
        )
    }
}

fn estimate(elapsed: Duration, completed: usize, total: usize) -> Option<Duration> {
    let remaining = total.checked_sub(completed)?;
    if completed == 0 || remaining == 0 {
        return None;
    }
    elapsed
        .checked_mul(remaining.try_into().ok()?)?
        .checked_div(completed.try_into().ok()?)
}

#[cfg(test)]
#[test]
fn estimate_requires_observed_work_and_a_remaining_plan() {
    let elapsed = Duration::from_secs(30);
    assert_eq!(estimate(elapsed, 2, 6), Some(Duration::from_secs(60)));
    assert_eq!(estimate(elapsed, 0, 6), None);
    assert_eq!(estimate(elapsed, 6, 6), None);
    assert_eq!(estimate(elapsed, 7, 6), None);
}
