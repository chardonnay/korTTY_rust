//! Per-run presentation state; never grants execution authority.

use std::cell::Cell;
use std::time::{Duration, Instant};

mod usage;

#[derive(Default)]
pub(super) struct StatusVisibility {
    finished_at: Option<Instant>,
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
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Message {
    Select(usize),
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
    pub(in crate::workspace::terminal_agent) model_usage: usage::Usage,
    first: usize,
    selected: Option<usize>,
    pub(in crate::workspace::terminal_agent) drawer: bool,
    pub(super) dock: Cell<Dock>,
    pub(super) status_visibility: StatusVisibility,
}

impl Presentation {
    pub(in crate::workspace::terminal_agent) fn finished(&mut self, now: Instant) {
        self.status_visibility.finished(now);
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

#[cfg(test)]
mod tests {
    use super::*;

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
