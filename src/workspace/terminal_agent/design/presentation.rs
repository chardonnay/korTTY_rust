//! Per-run presentation state; never grants execution authority.

use std::cell::Cell;
use std::time::{Duration, Instant};

use crate::terminal_agent::design::inline::{Kind, StepState, Transcript};

mod usage;
pub(in crate::workspace::terminal_agent) use usage::Usage;

use super::toast;

#[derive(Default)]
pub(super) struct StatusVisibility {
    pub(super) finished_at: Option<Instant>,
    manual: Option<bool>,
}

impl StatusVisibility {
    pub(super) fn finished(&mut self, now: Instant) {
        self.finished_at = Some(now);
        if self.manual == Some(true) {
            self.manual = None;
        }
    }

    pub(super) fn deadline(&self, seconds: u8) -> Option<Instant> {
        let finished = self.finished_at?;
        (seconds > 0 && self.manual.is_none())
            .then(|| finished + Duration::from_secs(seconds.into()))
    }

    pub(super) fn visible(&self, seconds: u8, now: Instant) -> bool {
        self.manual
            .unwrap_or_else(|| self.deadline(seconds).is_none_or(|deadline| now < deadline))
    }

    pub(super) fn toggle(&mut self, seconds: u8, now: Instant) {
        self.manual = Some(!self.visible(seconds, now));
    }

    pub(super) fn show(&mut self) {
        self.manual = Some(true);
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Message {
    Select(usize),
    SelectStep(u64),
    ToggleStepList,
    Previous,
    Next,
    ToggleDrawer,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum Dock {
    Side,
    #[default]
    Bar,
    Pill,
}

impl Dock {
    /// Matches the Java adaptive dock thresholds and hysteresis.
    pub(super) fn choose(self, columns: usize, rows: usize) -> Self {
        if columns == 0 || rows == 0 {
            return self;
        }
        if self == Self::Side && columns >= 142 && rows >= 27
            || self == Self::Pill && (columns < 78 || rows < 19)
        {
            return self;
        }
        if columns < 70 || rows < 16 {
            Self::Pill
        } else if columns >= 150 && rows >= 30 {
            Self::Side
        } else {
            Self::Bar
        }
    }
}

#[derive(Default)]
pub(in crate::workspace::terminal_agent) struct Presentation {
    pub(super) dock_resize: Option<(super::DockPreferences, u16)>,
    pub(in crate::workspace::terminal_agent) activity: super::activity::Panel,
    pub(in crate::workspace::terminal_agent) pip: super::pip::Panel,
    pub(in crate::workspace::terminal_agent) model_usage: Usage,
    first: usize,
    selected: Option<usize>,
    pub(in crate::workspace::terminal_agent) drawer: bool,
    pub(super) dock: Cell<Dock>,
    pub(super) status_visibility: StatusVisibility,
    pub(super) status_events: StatusEvents,
    pub(super) selected_step: Option<u64>,
    pub(super) step_list_open: bool,
}

impl Presentation {
    pub(in crate::workspace::terminal_agent) fn reveal_finished(&mut self, finished: Instant) {
        self.status_visibility.finished(finished);
        self.status_visibility.show();
    }

    pub(in crate::workspace::terminal_agent) fn finish_time(&self) -> Option<Instant> {
        self.status_visibility.finished_at
    }

    pub(in crate::workspace::terminal_agent) fn finished(&mut self, now: Instant) {
        self.status_visibility.finished(now);
    }

    pub(in crate::workspace::terminal_agent) fn sync_status(
        &mut self,
        transcript: &Transcript,
        finished: bool,
        waiting: bool,
        now: Instant,
    ) {
        self.status_events.sync(transcript, finished, now);
        self.status_events.track_review(waiting, now);
    }

    pub(in crate::workspace::terminal_agent) fn load_activity(
        &mut self,
        settings: &crate::workspace::tab_titles::WindowSettings,
    ) {
        self.activity = super::activity::Panel::load(settings);
        self.pip.folded = self.activity.collapsed;
    }

    pub(in crate::workspace::terminal_agent) fn review(&mut self, phase: super::super::Phase) {
        use super::super::Phase;
        if matches!(
            phase,
            Phase::ModelReview
                | Phase::ProbeApproval
                | Phase::Approval
                | Phase::Question
                | Phase::SudoPassword
                | Phase::PlanReady
                | Phase::PlanQuestions
                | Phase::PlanOptions
        ) {
            self.drawer = true;
        }
    }

    pub(super) fn selection(&self, len: usize) -> usize {
        self.selected
            .and_then(|selected| selected.checked_sub(self.first))
            .filter(|&index| index < len)
            .unwrap_or_else(|| len.saturating_sub(1))
    }

    pub(in crate::workspace::terminal_agent) fn identity(&self, index: usize) -> usize {
        self.first.saturating_add(index)
    }

    pub(in crate::workspace::terminal_agent) fn evict(&mut self) {
        self.first = self.first.saturating_add(1);
        if self.selected.is_some_and(|selected| selected < self.first) {
            self.selected = None;
        }
    }

    pub(in crate::workspace::terminal_agent) fn update(&mut self, message: Message, len: usize) {
        match message {
            Message::Select(identity)
                if identity
                    .checked_sub(self.first)
                    .is_some_and(|index| index < len) =>
            {
                self.selected = Some(identity);
                self.drawer = true;
            }
            Message::SelectStep(identity) => {
                self.selected_step = Some(identity);
                self.drawer = true;
            }
            Message::ToggleStepList => self.step_list_open = !self.step_list_open,
            Message::Previous if len > 0 => {
                self.selected = Some(self.identity(self.selection(len).saturating_sub(1)));
            }
            Message::Next if len > 0 => {
                let index = (self.selection(len) + 1).min(len - 1);
                // Reaching the newest entry resumes following incoming activity.
                self.selected = (index < len - 1).then(|| self.identity(index));
            }
            Message::ToggleDrawer => self.drawer = !self.drawer,
            _ => {}
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum Notice {
    Step(u64),
    Answer,
}

#[derive(Default)]
pub(super) struct StatusEvents {
    steps: Vec<(u64, Instant)>,
    answer: Option<Instant>,
    review: Option<Instant>,
}

impl StatusEvents {
    pub(super) fn sync(&mut self, transcript: &Transcript, finished: bool, now: Instant) {
        self.steps
            .retain(|(id, _)| transcript.steps.iter().any(|step| step.id == *id));
        for step in &transcript.steps {
            if step.state != StepState::Running
                && step.kind != Kind::Question
                && !self.steps.iter().any(|(id, _)| *id == step.id)
            {
                self.steps.push((step.id, now));
            }
        }
        if finished && !transcript.final_answer.is_empty() {
            self.answer.get_or_insert(now);
        }
    }

    pub(super) fn recent(
        &self,
        now: Instant,
        reduced_motion: bool,
    ) -> impl DoubleEndedIterator<Item = (Notice, toast::Frame)> {
        self.steps
            .iter()
            .map(|(id, at)| (Notice::Step(*id), *at))
            .chain(self.answer.map(|at| (Notice::Answer, at)))
            .filter_map(move |(event, at)| {
                toast::Frame::at(now.saturating_duration_since(at), reduced_motion)
                    .map(|frame| (event, frame))
            })
    }

    fn track_review(&mut self, waiting: bool, now: Instant) {
        self.review = waiting.then(|| self.review.unwrap_or(now));
    }

    pub(super) fn review_age(&self, now: Instant) -> Option<Duration> {
        self.review
            .map(|started| now.saturating_duration_since(started))
    }

    pub(super) fn ticking(&self, now: Instant, reduced_motion: bool) -> bool {
        self.recent(now, reduced_motion).next().is_some()
            || !reduced_motion && self.review_age(now).is_some_and(toast::review_animating)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_agent::design::inline::Step;

    #[test]
    fn completed_steps_expire_without_reappearing_and_the_final_answer_has_its_own_deadline() {
        let now = Instant::now();
        let mut events = StatusEvents::default();
        let mut trace = Transcript {
            steps: vec![
                Step {
                    id: 1,
                    kind: Kind::Action,
                    state: StepState::Running,
                    ..Step::default()
                },
                Step {
                    id: 2,
                    kind: Kind::Question,
                    ..Step::default()
                },
            ],
            ..Transcript::default()
        };
        events.sync(&trace, false, now);
        assert!(events.recent(now, true).next().is_none());
        trace.steps[0].state = StepState::Completed;
        let completed = now + Duration::from_secs(1);
        events.sync(&trace, false, completed);
        assert!(matches!(
            events.recent(completed, true).next(),
            Some((Notice::Step(1), frame)) if frame.remaining == toast::LIFETIME
        ));
        let expired = completed + toast::LIFETIME;
        trace.final_answer = "Done".into();
        events.sync(&trace, true, expired);
        assert!(matches!(
            events.recent(expired, true).collect::<Vec<_>>().as_slice(),
            [(Notice::Answer, frame)] if frame.remaining == toast::LIFETIME
        ));
        assert!(
            events
                .recent(expired + toast::LIFETIME, true)
                .next()
                .is_none()
        );
    }

    #[test]
    fn status_hides_at_its_deadline_and_can_be_shown_until_the_next_run() {
        let now = Instant::now();
        let mut status = StatusVisibility::default();
        assert!(status.visible(6, now));
        assert_eq!(status.deadline(6), None);
        status.finished(now);
        let deadline = now + Duration::from_secs(6);
        assert_eq!(status.deadline(6), Some(deadline));
        assert!(status.visible(6, deadline - Duration::from_millis(1)));
        assert!(!status.visible(6, deadline));
        assert!(status.visible(0, deadline));
        assert_eq!(status.deadline(0), None);
        status.toggle(6, deadline);
        assert!(status.visible(6, deadline + Duration::from_secs(60)));
        assert_eq!(status.deadline(6), None);
        status.toggle(6, deadline);
        assert!(!status.visible(0, deadline));
        assert!(StatusVisibility::default().visible(6, deadline));
    }

    #[test]
    fn delayed_selection_never_selects_a_different_evicted_activity() {
        let mut presentation = Presentation::default();
        presentation.update(Message::Select(0), 3);
        presentation.evict();
        presentation.update(Message::Select(0), 3);
        assert_eq!(presentation.selection(3), 2);
        presentation.update(Message::Select(1), 3);
        assert_eq!(presentation.selection(3), 0);
        presentation.update(Message::Next, 3);
        assert_eq!(presentation.selection(3), 1);
        presentation.update(Message::Next, 3);
        assert_eq!(presentation.selection(4), 3);
        presentation.update(Message::Previous, 0);
        assert_eq!(presentation.selection(0), 0);
    }

    #[test]
    fn adaptive_dock_preserves_java_thresholds_and_hysteresis() {
        assert_eq!(Dock::Bar.choose(149, 30), Dock::Bar);
        assert_eq!(Dock::Bar.choose(150, 30), Dock::Side);
        assert_eq!(Dock::Side.choose(142, 27), Dock::Side);
        assert_eq!(Dock::Side.choose(141, 27), Dock::Bar);
        assert_eq!(Dock::Bar.choose(69, 30), Dock::Pill);
        assert_eq!(Dock::Bar.choose(150, 15), Dock::Pill);
        assert_eq!(Dock::Pill.choose(77, 30), Dock::Pill);
        assert_eq!(Dock::Pill.choose(150, 18), Dock::Pill);
        assert_eq!(Dock::Pill.choose(78, 19), Dock::Bar);
        assert_eq!(Dock::Side.choose(0, 0), Dock::Side);
    }
}
