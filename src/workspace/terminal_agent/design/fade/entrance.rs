use std::cell::Cell;
use std::time::{Duration, Instant};

use iced::{Rectangle, Transformation};

use super::super::presentation::ease_both;

const FADE: Duration = Duration::from_millis(220);
const SCALE: Duration = Duration::from_millis(240);
const FRAME: Duration = Duration::from_millis(16);
const INITIAL_SCALE: f32 = 0.94;

#[derive(Default)]
pub(in crate::workspace::terminal_agent::design) struct Entrance(Cell<Visibility>);

#[derive(Clone, Copy, Default)]
enum Visibility {
    #[default]
    Hidden,
    Entering {
        start: Instant,
        age: Duration,
    },
    Shown,
}

#[derive(Debug, PartialEq)]
pub(super) struct Frame {
    pub(super) opacity: f32,
    scale: f32,
}

impl Frame {
    pub(super) const SHOWN: Self = Self {
        opacity: 1.0,
        scale: 1.0,
    };

    pub(super) fn transform(&self, bounds: Rectangle) -> Transformation {
        if self.scale == 1.0 {
            return Transformation::IDENTITY;
        }
        let center = bounds.center();
        let inset = 1.0 - self.scale;
        Transformation::translate(center.x * inset, center.y * inset)
            * Transformation::scale(self.scale)
    }
}

impl Entrance {
    pub(in crate::workspace::terminal_agent::design) fn hide(&self) {
        self.0.set(Visibility::Hidden);
    }

    pub(in crate::workspace::terminal_agent::design) fn settle(&self) {
        self.0.set(Visibility::Shown);
    }

    pub(super) fn redraw(&self, now: Instant) -> Option<Instant> {
        let start = match self.0.get() {
            Visibility::Shown => return None,
            Visibility::Entering { start, .. } => start,
            Visibility::Hidden => now,
        };
        let age = now.saturating_duration_since(start);
        if age >= SCALE {
            self.settle();
            return None;
        }
        self.0.set(Visibility::Entering { start, age });
        Some((now + FRAME).min(start + SCALE))
    }

    pub(super) fn frame(&self) -> Frame {
        let age = match self.0.get() {
            Visibility::Shown => return Frame::SHOWN,
            Visibility::Entering { age, .. } => age,
            Visibility::Hidden => Duration::ZERO,
        };
        Frame {
            opacity: ease_both(age, FADE),
            scale: INITIAL_SCALE + (1.0 - INITIAL_SCALE) * ease_both(age, SCALE),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::Size;

    #[test]
    fn hud_fades_and_scales_from_its_center_each_time_it_is_shown() {
        let entrance = Entrance::default();
        let now = Instant::now();
        let bounds = Rectangle {
            x: 24.0,
            y: 35.0,
            width: 300.0,
            height: 100.0,
        };
        assert_eq!(entrance.redraw(now), Some(now + FRAME));
        let first = entrance.frame();
        assert_eq!(first.opacity, 0.0);
        let scaled = bounds * first.transform(bounds);
        assert_eq!(scaled.center(), bounds.center());
        assert_eq!(scaled.size(), Size::new(282.0, 94.0));
        assert_eq!(entrance.redraw(now + FADE), Some(now + FADE + FRAME));
        let faded_in = entrance.frame();
        assert_eq!(faded_in.opacity, 1.0);
        assert!(faded_in.scale < 1.0);
        assert_eq!(entrance.redraw(now + SCALE), None);
        assert_eq!(entrance.frame(), Frame::SHOWN);

        entrance.hide();
        assert_eq!(entrance.redraw(now + SCALE), Some(now + SCALE + FRAME));
        assert_eq!(entrance.frame(), first);
        entrance.settle();
        assert_eq!(entrance.redraw(now + SCALE), None);
        assert_eq!(entrance.frame(), Frame::SHOWN);
    }
}
