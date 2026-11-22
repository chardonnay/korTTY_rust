//! Per-run presentation state; never grants execution authority.

use std::cell::Cell;
use std::time::{Duration, Instant};

use crate::terminal_agent::design::inline::{Kind, StepState, Transcript};

mod usage;
pub(in crate::workspace::terminal_agent) use usage::Usage;

use super::toast;

const STATUS_FADE: Duration = Duration::from_millis(250);
const FRAME: Duration = Duration::from_millis(16);

#[derive(Clone, Copy)]
pub(in crate::workspace) struct StatusTiming {
    deadline: Option<Instant>,
    fade: Duration,
}

impl StatusTiming {
    pub(in crate::workspace) fn new(deadline: Option<Instant>, reduced_motion: bool) -> Self {
        Self {
            deadline,
            fade: if reduced_motion {
                Duration::ZERO
            } else {
                STATUS_FADE
            },
        }
    }

    pub(in crate::workspace) fn opacity(self, now: Instant) -> Option<f32> {
        let Some(start) = self.deadline.filter(|&start| now >= start) else {
            return Some(1.0);
        };
        let age = now.saturating_duration_since(start);
        if age >= self.fade {
            return None;
        }

        // The Java transition eases in and out using 20% acceleration and braking ramps.
        let phase = age.as_secs_f32() / self.fade.as_secs_f32();
        const RAMP: f32 = 0.2;
        const SPEED: f32 = 1.0 / (1.0 - RAMP);
        let ramp = |t: f32| SPEED * t * t / (2.0 * RAMP);
        Some(if phase < RAMP {
            1.0 - ramp(phase)
        } else if phase > 1.0 - RAMP {
            ramp(1.0 - phase)
        } else {
            1.0 - SPEED * (phase - RAMP / 2.0)
        })
    }

    pub(in crate::workspace) fn next_frame(self, now: Instant) -> Option<Instant> {
        let start = self.deadline?;
        if now < start {
            return Some(start);
        }
        let end = start + self.fade;
        (now < end).then(|| (now + FRAME).min(end))
    }

    pub(super) fn clock_interval(self, now: Instant) -> Option<u64> {
        let start = self.deadline?;
        self.next_frame(now)?;
        Some(
            if !self.fade.is_zero() && start <= now + Duration::from_millis(100) {
                FRAME.as_millis() as u64
            } else {
                100
            },
        )
    }
}

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

    pub(super) fn opacity(&self, seconds: u8, now: Instant, reduced_motion: bool) -> Option<f32> {
        if self.manual == Some(false) {
            return None;
        }
        StatusTiming::new(self.deadline(seconds), reduced_motion).opacity(now)
    }

    pub(super) fn toggle(&mut self, seconds: u8, now: Instant, reduced_motion: bool) {
        self.manual = Some(self.opacity(seconds, now, reduced_motion).is_none());
    }

    pub(super) fn show(&mut self) {
        self.manual = Some(true);
    }

    pub(super) fn hide(&mut self) {
        self.manual = Some(false);
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Message {
    Select(usize),
    SelectStep(u64),
    ConversationOutput(super::conversation::OutputKey),
    EarlierConversationStep(
        super::conversation::OutputKey,
        super::log_table::Surface,
        super::actions::Item,
    ),
    LogTable(super::log_table::Message),
    ToggleStepList,
    BrowseCards(usize),
    ScrollCards {
        direction: i8,
        review: bool,
    },
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
    card_stack_back: usize,
    pub(in crate::workspace::terminal_agent) drawer: bool,
    pub(super) dock: Cell<Dock>,
    pub(super) status_visibility: StatusVisibility,
    pub(super) status_events: StatusEvents,
    pub(super) selected_step: Option<u64>,
    pub(super) conversation_output: std::collections::HashSet<super::conversation::OutputKey>,
    pub(super) step_list_open: bool,
    pub(super) log_table: super::log_table::State,
    pub(super) tmux_started: Option<Instant>,
    pub(super) review_started: Option<Instant>,
    pub(super) timeline_viewport: std::cell::OnceCell<iced::widget::Id>,
    pub(super) plan_progress: super::plan_execution::Progress,
    pub(in crate::workspace::terminal_agent) dock_hidden: bool,
}

impl Presentation {
    pub(in crate::workspace::terminal_agent) fn set_dock_hidden(&mut self, hidden: bool) {
        self.dock_hidden = hidden;
        if hidden {
            self.dock_resize = None;
            self.activity.preview = None;
        }
    }

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
        self.log_table.sync(transcript, now);
        self.plan_progress.sync(transcript, finished, now);
        self.tmux_started.get_or_insert(now);
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
        self.review_started =
            matches!(phase, Phase::ProbeApproval | Phase::Approval).then(Instant::now);
        if matches!(
            phase,
            Phase::ModelReview
                | Phase::ProbeApproval
                | Phase::Approval
                | Phase::Question
                | Phase::SudoPreflight
                | Phase::SudoPassword
                | Phase::PlanReady
                | Phase::PlanImplementationReview
                | Phase::PlanQuestions
                | Phase::PlanOptions
                | Phase::TriageChoice
        ) {
            self.drawer = true;
        }
    }

    pub(in crate::workspace::terminal_agent) fn close_status_run_menu(&mut self) {
        self.log_table.close_status_run_menu();
    }

    pub(super) fn selection(&self, len: usize) -> usize {
        self.selected
            .and_then(|selected| selected.checked_sub(self.first))
            .filter(|&index| index < len)
            .unwrap_or_else(|| len.saturating_sub(1))
    }

    pub(super) fn card_stack_back(&self, len: usize) -> usize {
        self.card_stack_back.min(len.saturating_sub(1))
    }

    pub(super) fn card_stack_browsing(&self) -> bool {
        self.card_stack_back > 0
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
            Message::ConversationOutput(key) => {
                if !self.conversation_output.remove(&key) {
                    self.conversation_output.insert(key);
                }
            }
            Message::LogTable(message) => self.log_table.update(message),
            Message::ToggleStepList => self.step_list_open = !self.step_list_open,
            Message::BrowseCards(back) => self.card_stack_back = back.min(len),
            Message::ScrollCards { direction, review } => {
                let count = len + usize::from(review);
                self.card_stack_back = self
                    .card_stack_back(count)
                    .saturating_add_signed(isize::from(direction))
                    .min(count.saturating_sub(1));
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
    fn status_fades_after_its_deadline_and_manual_visibility_is_immediate() {
        let now = Instant::now();
        let mut status = StatusVisibility::default();
        assert_eq!(status.opacity(6, now, false), Some(1.0));
        assert_eq!(status.deadline(6), None);
        status.finished(now);
        let deadline = now + Duration::from_secs(6);
        let end = deadline + STATUS_FADE;
        assert_eq!(status.deadline(6), Some(deadline));
        assert_eq!(
            status.opacity(6, deadline - Duration::from_millis(1), false),
            Some(1.0)
        );
        assert_eq!(status.opacity(6, deadline, false), Some(1.0));
        assert_eq!(
            status.opacity(6, deadline + STATUS_FADE / 2, false),
            Some(0.5)
        );
        assert_eq!(status.opacity(6, deadline, true), None);
        assert_eq!(status.opacity(6, end, false), None);
        assert_eq!(status.opacity(0, end, false), Some(1.0));
        assert_eq!(status.deadline(0), None);
        status.toggle(6, end, false);
        assert_eq!(
            status.opacity(6, end + Duration::from_secs(60), false),
            Some(1.0)
        );
        assert_eq!(status.deadline(6), None);
        status.toggle(6, end, false);
        assert_eq!(status.opacity(0, end, false), None);
        assert_eq!(
            StatusVisibility::default().opacity(6, end, false),
            Some(1.0)
        );
    }

    #[test]
    fn status_fade_clock_wakes_at_the_boundaries() {
        let deadline = Instant::now();
        let before = deadline - Duration::from_millis(1);
        let end = deadline + STATUS_FADE;
        let fade = StatusTiming::new(Some(deadline), false);
        assert_eq!(fade.next_frame(before), Some(deadline));
        assert_eq!(fade.next_frame(deadline), Some(deadline + FRAME));
        assert_eq!(fade.next_frame(end - Duration::from_millis(1)), Some(end));
        assert_eq!(fade.next_frame(end), None);
        assert_eq!(fade.clock_interval(before), Some(16));
        assert_eq!(fade.clock_interval(end), None);
        let still = StatusTiming::new(Some(deadline), true);
        assert_eq!(still.next_frame(before), Some(deadline));
        assert_eq!(still.next_frame(deadline), None);
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
