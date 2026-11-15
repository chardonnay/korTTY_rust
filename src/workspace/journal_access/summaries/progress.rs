use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use iced::widget::{button, column, progress_bar, row, scrollable, text};
use iced::{Element, Length, Size, Task, window};

use crate::local_ai::Profile;
use crate::session_journal::summary_ai::{Stage, Work};
use crate::session_journal::summary_job::Mode;
use crate::workspace::{coding_agents::duration_text, dialog_placement::Kind};

use super::{
    Active, Desktop, Event, JournalMessage, PROGRESS_TITLE, Receipt, State, Workspace,
    summary_live_status,
};

pub(super) struct Details {
    pub(super) id: window::Id,
    batch: String,
    status: Option<String>,
    completed: bool,
}

const CREEP_CAP: f64 = 0.95;
const MAX_PROFILE_RATES: usize = 64;

#[derive(Clone, Copy, Default)]
struct Measurement {
    units: f64,
    seconds: f64,
}

impl Measurement {
    fn add(&mut self, other: Self) {
        self.units += other.units;
        self.seconds += other.seconds;
    }

    fn rate(self) -> Option<f64> {
        (self.units > 0.0 && self.seconds > 0.0).then(|| self.seconds / self.units)
    }
}

#[derive(Clone, Copy)]
struct CurrentWork {
    stage: Stage,
    weight: f64,
    started: Instant,
}

#[derive(Clone, Default)]
pub(super) struct WorkProgress {
    remaining: [f64; 4],
    finished: [Measurement; 2],
    current: Option<CurrentWork>,
    has_plan: bool,
}

fn plan_slot(stage: Stage) -> Option<usize> {
    match stage {
        Stage::Window => Some(0),
        Stage::Closing => Some(1),
        Stage::Title => Some(2),
        Stage::Screenshot => Some(3),
        Stage::Checking => None,
    }
}

fn rate_slot(stage: Stage) -> usize {
    usize::from(stage == Stage::Screenshot)
}

impl WorkProgress {
    pub(super) fn update(&mut self, work: Work) {
        match work {
            Work::Plan { stage, weight } => {
                if let Some(slot) = plan_slot(stage) {
                    self.remaining[slot] = weight;
                    self.has_plan = true;
                }
            }
            Work::Started { stage, weight, at } => {
                if let Some(slot) = plan_slot(stage) {
                    self.remaining[slot] = (self.remaining[slot] - weight).max(0.0);
                    self.current = Some(CurrentWork {
                        stage,
                        weight,
                        started: at,
                    });
                }
            }
            Work::Finished {
                stage,
                weight,
                at,
                elapsed,
            } => {
                if self.current.is_some_and(|current| {
                    current.stage == stage && current.weight == weight && current.started <= at
                }) {
                    self.current = None;
                    self.finished[rate_slot(stage)].add(Measurement {
                        units: weight,
                        seconds: elapsed.as_secs_f64(),
                    });
                }
            }
        }
    }

    fn measurements(&self, distinct_vision: bool) -> [Measurement; 2] {
        let [mut text, vision] = self.finished;
        if distinct_vision {
            [text, vision]
        } else {
            text.add(vision);
            [text, Measurement::default()]
        }
    }

    fn estimate(
        &self,
        now: Instant,
        past: [Measurement; 2],
        following: f64,
        distinct_vision: bool,
        prior: [Option<f64>; 2],
    ) -> Option<Estimate> {
        if !self.has_plan
            && self.current.is_none()
            && self.finished.iter().all(|work| work.units == 0.0)
        {
            return None;
        }
        let mut measured = self.measurements(distinct_vision);
        for (current, past) in measured.iter_mut().zip(past) {
            current.add(past);
        }
        let rates = [
            measured[0].rate().or(prior[0]),
            measured[1].rate().or(prior[1]),
        ];
        let done = measured[0].units + measured[1].units;
        let mut future = [self.remaining.iter().sum::<f64>() + following, 0.0];
        if distinct_vision {
            future[1] = self.remaining[3];
            future[0] -= future[1];
        }
        let pending = future[0] + future[1];
        let current_units = self.current.map_or(0.0, |current| current.weight);
        let mut creep = 0.0;
        let mut remaining = [Some(0.0), Some(0.0)];
        for (index, weight) in future.into_iter().enumerate() {
            if weight > f64::EPSILON {
                remaining[index] = rates[index].map(|rate| rate * weight);
            }
        }
        if let Some(current) = self.current {
            let index = if distinct_vision {
                rate_slot(current.stage)
            } else {
                0
            };
            let current_remaining = rates[index].map(|rate| {
                let expected = current.weight * rate;
                let elapsed = now.saturating_duration_since(current.started).as_secs_f64();
                creep = (elapsed / expected).min(CREEP_CAP);
                (expected - elapsed).max(0.0)
            });
            remaining[index] = remaining[index]
                .zip(current_remaining)
                .map(|(future, current)| future + current);
        }
        let total = done + current_units + pending;
        if total <= f64::EPSILON {
            return None;
        }
        let fraction = (done + creep * current_units) / total;
        let remaining = (current_units + pending > f64::EPSILON)
            .then(|| Duration::try_from_secs_f64(remaining[0]? + remaining[1]?).ok())
            .flatten();
        Some(Estimate {
            fraction: fraction.clamp(0.0, 1.0),
            remaining,
        })
    }
}

#[derive(Clone, Copy)]
pub(super) struct Estimate {
    fraction: f64,
    pub(super) remaining: Option<Duration>,
}

impl Estimate {
    pub(super) fn active_percent(self) -> u8 {
        (self.fraction * 100.0).floor().min(99.0) as u8
    }
}

#[derive(Default)]
pub(super) struct ProfileRates(Vec<(Profile, f64)>);

impl ProfileRates {
    fn get(&self, profile: Option<&Profile>) -> Option<f64> {
        let profile = profile?;
        self.0
            .iter()
            .find(|(key, _)| key == profile)
            .map(|(_, rate)| *rate)
    }

    fn remember(&mut self, profile: Option<&Profile>, work: Measurement) {
        if let Some((profile, rate)) = profile.zip(work.rate()) {
            self.0.retain(|(key, _)| key != profile);
            if self.0.len() == MAX_PROFILE_RATES {
                self.0.remove(0);
            }
            self.0.push((profile.clone(), rate));
        }
    }
}

pub(super) struct BatchProgress {
    started: Instant,
    measurements: [Measurement; 2],
    following: HashMap<String, f64>,
}

impl Details {
    pub(super) fn matches(&self, id: window::Id, batch: &str) -> bool {
        self.id == id && self.batch == batch
    }
}

impl State {
    pub(super) fn following_work(&self, active: &Receipt) -> (HashMap<String, f64>, Vec<Receipt>) {
        let cached = self
            .batch_progress
            .get(&active.batch)
            .map(|batch| &batch.following);
        let mut known = HashMap::new();
        let mut unplanned = Vec::new();
        if matches!(active.mode, Mode::Reevaluate | Mode::CatchUp) {
            for receipt in self.queue.pending().iter().filter(|receipt| {
                receipt.batch == active.batch
                    && receipt.generation != active.generation
                    && !self.paused.contains(&receipt.generation)
                    && receipt.mode == active.mode
                    && receipt.profile == active.profile
                    && receipt.full_output == active.full_output
                    && receipt.evidence_limits == active.evidence_limits
                    && receipt.source.directory().parent() == active.source.directory().parent()
                    && (!receipt.reviewed() || self.queue.authorizes_reviewed(receipt))
            }) {
                if let Some(weight) = cached.and_then(|planned| planned.get(&receipt.generation)) {
                    known.insert(receipt.generation.clone(), *weight);
                } else {
                    unplanned.push(receipt.clone());
                }
            }
        }
        (known, unplanned)
    }

    pub(super) fn estimate(&self, active: &Active, now: Instant) -> Option<Estimate> {
        let batch = self.batch_progress.get(&active.receipt.batch);
        let past = batch.map_or([Measurement::default(); 2], |batch| batch.measurements);
        let following = self
            .queue
            .pending()
            .iter()
            .filter_map(|receipt| {
                (receipt.batch == active.receipt.batch
                    && receipt.generation != active.receipt.generation
                    && !self.paused.contains(&receipt.generation))
                .then(|| active.visible.following.get(&receipt.generation).copied())
                .flatten()
            })
            .sum();
        let vision = active.vision_profile.as_ref();
        active.visible.work.estimate(
            now,
            past,
            following,
            vision.is_some(),
            [
                self.work_rates.get(active.profile.as_ref()),
                self.work_rates.get(vision),
            ],
        )
    }

    pub(super) fn progress_status(&self, active: &Active, now: Instant) -> String {
        let mut status = format!(
            "{} · {} activity windows processed",
            active.receipt.title, active.visible.outcome.windows
        );
        if let Some(estimate) = self.estimate(active, now) {
            status.push_str(&format!(
                " · {}% · Elapsed {}",
                estimate.active_percent(),
                duration_text(self.elapsed(active, now).as_secs())
            ));
            if let Some(remaining) = estimate.remaining {
                status.push_str(&format!(" · About {} remaining", remaining_text(remaining)));
            }
        }
        status
    }

    pub(super) fn finish_work(&mut self, active: &Active) {
        let snapshot = active
            .progress
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let vision = active.vision_profile.as_ref();
        let measured = snapshot.work.measurements(vision.is_some());
        let mut batch =
            self.batch_progress
                .remove(&active.receipt.batch)
                .unwrap_or(BatchProgress {
                    started: active.started,
                    measurements: [Measurement::default(); 2],
                    following: HashMap::new(),
                });
        for (total, work) in batch.measurements.iter_mut().zip(measured) {
            total.add(work);
        }
        let pending = self.queue.pending().iter().any(|receipt| {
            receipt.batch == active.receipt.batch
                && receipt.generation != active.receipt.generation
                && !self.paused.contains(&receipt.generation)
        });
        if !active.periodic && pending && !active.stop.load(Ordering::Acquire) {
            batch.following.clone_from(&snapshot.following);
            self.batch_progress
                .insert(active.receipt.batch.clone(), batch);
        } else {
            self.work_rates
                .remember(active.profile.as_ref(), batch.measurements[0]);
            self.work_rates.remember(vision, batch.measurements[1]);
        }
    }

    fn elapsed(&self, active: &Active, now: Instant) -> Duration {
        let started = self
            .batch_progress
            .get(&active.receipt.batch)
            .map_or(active.started, |batch| batch.started);
        now.saturating_duration_since(started)
    }

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
            details.completed = false;
        }
    }

    pub(super) fn complete_details(&mut self, batch: &str) {
        if let Some(details) = self
            .details
            .as_mut()
            .filter(|details| details.batch == batch)
            && !self
                .queue
                .pending()
                .iter()
                .any(|receipt| receipt.batch == batch)
        {
            details.completed = true;
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
                details.completed = false;
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
            completed: false,
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
            let now = Instant::now();
            let elapsed = state.elapsed(active, now);
            content = content.push(text(match outcome.planned_windows {
                Some(total) => format!("{} of {total} summaries written", outcome.windows),
                None => format!("{} summaries written", outcome.windows),
            }));
            if let Some(estimate) = state.estimate(active, now) {
                content = content.push(progress_bar(
                    0.0..=100.0,
                    f32::from(estimate.active_percent()),
                ));
                if let Some(eta) = estimate.remaining {
                    content = content.push(text(format!(
                        "Estimated time remaining · {}",
                        remaining_text(eta)
                    )));
                }
            }
            content = content.push(text(format!(
                "Elapsed · {}",
                duration_text(elapsed.as_secs())
            )));
            if let Some(preview) = &active.visible.preview {
                content = content.push(text(summary_live_status(preview)));
            } else if !active.visible.work.has_plan && outcome.windows == 0 {
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
        if details.completed && active.is_none() && waiting.is_none() {
            content = content.push(progress_bar(0.0..=100.0, 100.0));
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

fn remaining_text(remaining: Duration) -> String {
    let seconds = remaining.as_secs();
    if seconds < 60 {
        return "under a minute".into();
    }
    let minutes = seconds.saturating_add(30) / 60;
    if minutes >= 90 {
        return format!("{} h {:02} min", minutes / 60, minutes % 60);
    }
    format!("{minutes} min")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn begin(work: &mut WorkProgress, stage: Stage, weight: f64, at: Instant) {
        work.update(Work::Started { stage, weight, at });
    }

    fn finish(work: &mut WorkProgress, stage: Stage, weight: f64, at: Instant, seconds: u64) {
        work.update(Work::Finished {
            stage,
            weight,
            at,
            elapsed: Duration::from_secs(seconds),
        });
    }

    #[test]
    fn journal_weighted_progress_includes_later_stages_and_next_journal() {
        let mut work = WorkProgress::default();
        let start = Instant::now();
        let at = |seconds| start + Duration::from_secs(seconds);
        for (stage, weight) in [
            (Stage::Window, 2.0),
            (Stage::Closing, 1.3),
            (Stage::Title, 1.0),
            (Stage::Screenshot, 1.5),
        ] {
            work.update(Work::Plan { stage, weight });
        }
        let prior = [Some(4.0), None];
        let past = [Measurement::default(); 2];
        let estimate = work
            .estimate(start, past, 3.0, false, [None, None])
            .unwrap();
        assert_eq!(estimate.active_percent(), 0);
        assert!(estimate.remaining.is_none());
        let warmed = work.estimate(start, past, 3.0, false, prior).unwrap();
        assert_eq!(warmed.remaining.unwrap().as_secs_f64(), 35.2);
        begin(&mut work, Stage::Window, 2.0, at(15));
        finish(&mut work, Stage::Window, 2.0, at(35), 10);
        begin(&mut work, Stage::Screenshot, 1.5, at(40));
        let timing = work.estimate(at(80), past, 3.0, false, prior).unwrap();
        assert_eq!(timing.remaining.unwrap().as_secs_f64(), 26.5);
        assert_eq!(timing.active_percent(), 38);
        finish(&mut work, Stage::Screenshot, 1.5, at(80), 5);
        work.update(Work::Plan {
            stage: Stage::Title,
            weight: 0.0,
        });
        begin(&mut work, Stage::Closing, 1.3, at(80));
        let closing = work.estimate(at(81), past, 3.0, false, prior).unwrap();
        assert!(closing.remaining.unwrap() > Duration::from_secs(10));
        assert!(closing.active_percent() < 99);
    }

    #[test]
    fn journal_weighted_progress_keeps_vision_and_text_rates_separate() {
        let mut work = WorkProgress::default();
        let now = Instant::now();
        work.update(Work::Plan {
            stage: Stage::Closing,
            weight: 1.3,
        });
        work.update(Work::Plan {
            stage: Stage::Screenshot,
            weight: 1.5,
        });
        begin(&mut work, Stage::Screenshot, 1.5, now);
        let past = [Measurement::default(); 2];
        let cold_vision = work
            .estimate(now, past, 0.0, true, [Some(2.0), None])
            .unwrap();
        assert_eq!(cold_vision.remaining, None);
        let warmed = work
            .estimate(now, past, 0.0, true, [Some(2.0), Some(20.0)])
            .unwrap();
        assert_eq!(warmed.remaining, Some(Duration::from_millis(32_600)));
        finish(
            &mut work,
            Stage::Screenshot,
            1.5,
            now + Duration::from_secs(15),
            15,
        );
        let [text, vision] = work.measurements(true);
        assert_eq!(text.rate(), None);
        assert_eq!(vision.rate(), Some(10.0));
        let [together, unused] = work.measurements(false);
        assert_eq!(together.rate(), vision.rate());
        assert_eq!(unused.rate(), None);
        assert_eq!(remaining_text(Duration::from_secs(59)), "under a minute");
        assert_eq!(remaining_text(Duration::from_secs(119)), "2 min");
        assert_eq!(remaining_text(Duration::from_secs(5_410)), "1 h 30 min");
    }
}
