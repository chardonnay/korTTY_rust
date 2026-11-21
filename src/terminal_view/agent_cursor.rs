//! Non-interactive drawing and keyboard capture for an exact command review.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use iced::font::Style;
use iced::keyboard::{self, key::Named};
use iced::widget::canvas::{self, Geometry};
use iced::{Color, Font, Pixels, Point, Rectangle, Renderer, Size, window};
use kortty_terminal::Cursor;

use super::{Metrics, TerminalView, default_background};
use crate::session::SessionId;

const TYPE_STEP: Duration = Duration::from_millis(18);
const GLOW_STEP: Duration = Duration::from_millis(33);
const GLOW_HALF_CYCLE: Duration = Duration::from_millis(700);
const FADE_DURATION: Duration = Duration::from_millis(180);
const FADE_STEP: Duration = Duration::from_millis(16);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Binding {
    pub owner: window::Id,
    pub session: SessionId,
    pub stream: u64,
    pub run: window::Id,
    pub epoch: u64,
    pub screen_revision: u64,
    pub input_revision: u64,
}

impl Binding {
    fn same_review(self, other: Self) -> bool {
        self.owner == other.owner
            && self.session == other.session
            && self.stream == other.stream
            && self.run == other.run
            && self.epoch == other.epoch
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Message {
    Key(Binding, Key, Decision),
    Consumed(Key),
    Edited(Binding, Result<(), &'static str>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Key {
    Enter,
    Tab,
    Escape,
    One,
    Two,
    Three,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Decision {
    ApproveOnce,
    ApproveRemaining,
    Dismiss,
    Edit,
}

impl Decision {
    pub(super) fn approves_command(self) -> bool {
        matches!(self, Self::ApproveOnce | Self::ApproveRemaining)
    }
}

impl Key {
    pub(crate) fn from_key(key: &keyboard::Key) -> Option<Self> {
        match key {
            keyboard::Key::Named(Named::Enter) => Some(Self::Enter),
            keyboard::Key::Named(Named::Tab) => Some(Self::Tab),
            keyboard::Key::Named(Named::Escape) => Some(Self::Escape),
            keyboard::Key::Character(value) if value == "1" => Some(Self::One),
            keyboard::Key::Character(value) if value == "2" => Some(Self::Two),
            keyboard::Key::Character(value) if value == "3" => Some(Self::Three),
            _ => None,
        }
    }

    pub(crate) fn mask(self) -> u8 {
        1 << self as u8
    }

    pub(crate) fn decision(self, remaining_allowed: bool) -> Option<Decision> {
        match self {
            Self::Enter | Self::One => Some(Decision::ApproveOnce),
            Self::Tab => Some(Decision::Edit),
            Self::Two if remaining_allowed => Some(Decision::ApproveRemaining),
            Self::Escape | Self::Two => Some(Decision::Dismiss),
            Self::Three if remaining_allowed => Some(Decision::Dismiss),
            Self::Three => None,
        }
    }
}

pub(crate) struct Review<'a> {
    pub binding: Binding,
    pub command: &'a str,
    pub destination: String,
    pub inline: bool,
    pub editable: bool,
    pub remaining_allowed: bool,
    pub prompt: Option<crate::session::ShortcutPrompt>,
}

pub(crate) struct Surface<'a> {
    pub review: Option<Review<'a>>,
    pub departure: Option<Departure>,
    pub motion: &'a RefCell<Motion>,
}

pub(super) struct Position {
    ghost: Point,
    hint: Point,
    footer: f32,
}

#[derive(Clone, Copy)]
pub(crate) struct Departure {
    pub binding: Binding,
    pub fade: Fade,
}

#[derive(Clone, Copy)]
pub(crate) struct Fade(Instant);

impl Fade {
    pub(crate) fn new(now: Instant) -> Self {
        Self(now)
    }

    pub(crate) fn opacity(self, now: Instant) -> Option<f32> {
        let age = now.saturating_duration_since(self.0);
        (age < FADE_DURATION).then(|| 1.0 - age.as_secs_f32() / FADE_DURATION.as_secs_f32())
    }

    pub(crate) fn next_frame(self, now: Instant) -> Option<Instant> {
        self.opacity(now)
            .map(|_| (now + FADE_STEP).min(self.0 + FADE_DURATION))
    }
}

#[derive(Default)]
pub(crate) struct Motion(Option<ActiveMotion>);

struct ActiveMotion {
    binding: Binding,
    started: Instant,
    frame: MotionFrame,
    reveal_requested: bool,
    painted: Option<PaintedReview>,
}

struct PaintedReview {
    binding: Binding,
    command: String,
    destination: String,
    hint: &'static str,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct MotionFrame {
    age: Option<Duration>,
    revealed: bool,
}

impl Motion {
    fn follow(&mut self, binding: Binding, now: Instant) -> &mut ActiveMotion {
        if self
            .0
            .as_ref()
            .is_some_and(|active| !active.binding.same_review(binding))
        {
            self.0 = None;
        }
        self.0.get_or_insert_with(|| ActiveMotion {
            binding,
            started: now,
            frame: MotionFrame::animated(Duration::ZERO),
            reveal_requested: false,
            painted: None,
        })
    }

    pub(super) fn advance(
        &mut self,
        visible: Option<&Review<'_>>,
        departure: Option<Departure>,
        now: Instant,
        reduced: bool,
    ) -> Option<Instant> {
        let Some(review) = visible.filter(|_| !reduced) else {
            if !reduced
                && let Some(departure) = departure
                && self.departing(departure).is_some()
                && let Some(next) = departure.fade.next_frame(now)
            {
                return Some(next);
            }
            self.0 = None;
            return None;
        };
        let active = self.follow(review.binding, now);
        active.frame = MotionFrame {
            age: Some(now.saturating_duration_since(active.started)),
            revealed: active.reveal_requested,
        };
        let painted = active.painted.get_or_insert_with(|| PaintedReview {
            binding: review.binding,
            command: review.command.into(),
            destination: review.destination.clone(),
            hint: review.hint(),
        });
        painted.binding = review.binding;
        if painted.command != review.command {
            review.command.clone_into(&mut painted.command);
        }
        if painted.destination != review.destination {
            painted.destination.clone_from(&review.destination);
        }
        painted.hint = review.hint();
        Some(now + active.frame.interval(review.command))
    }

    fn departing(&self, departure: Departure) -> Option<(&ActiveMotion, &PaintedReview)> {
        let active = self.0.as_ref()?;
        let painted = active.painted.as_ref()?;
        (painted.binding.same_review(departure.binding)
            && painted.binding.input_revision == departure.binding.input_revision)
            .then_some((active, painted))
    }

    pub(super) fn draw_departure(
        &self,
        view: &TerminalView<'_>,
        renderer: &Renderer,
        size: Size,
        departure: Departure,
        now: Instant,
    ) -> Option<Geometry> {
        let (active, painted) = self.departing(departure)?;
        let opacity = departure.fade.opacity(now)?;
        let suggestion = Suggestion {
            command: &painted.command,
            destination: &painted.destination,
            hint: painted.hint,
        };
        let cursor = view.projected_cursor()?;
        let cell = Metrics::with_font(view.font_size, view.font);
        let position = suggestion.position(size, cursor, cell)?;
        Some(suggestion.draw(view, renderer, size, Some(position), active.frame, opacity))
    }

    pub(super) fn frame(&self, binding: Binding, reduced: bool) -> MotionFrame {
        if reduced {
            return MotionFrame::default();
        }
        self.0
            .as_ref()
            .filter(|active| active.binding.same_review(binding))
            .map_or_else(
                || MotionFrame::animated(Duration::ZERO),
                |active| active.frame,
            )
    }

    pub(super) fn ready_to_approve(&mut self, review: &Review<'_>, reduced: bool) -> bool {
        if self.frame(review.binding, reduced).complete(review.command) {
            return true;
        }
        // A key can reveal a suggestion immediately. Approval requires a new
        // press after the full command has been rendered.
        self.follow(review.binding, Instant::now()).reveal_requested = true;
        false
    }
}

impl MotionFrame {
    pub(crate) fn animated(age: Duration) -> Self {
        Self {
            age: Some(age),
            revealed: false,
        }
    }

    pub(crate) fn command(self, command: &str) -> &str {
        let Some(age) = self.age.filter(|_| !self.revealed) else {
            return command;
        };
        let count = (age.as_millis() / TYPE_STEP.as_millis()) as usize;
        command
            .char_indices()
            .nth(count)
            .map_or(command, |(end, _)| &command[..end])
    }

    fn complete(self, command: &str) -> bool {
        self.command(command).len() == command.len()
    }

    pub(crate) fn interval(self, command: &str) -> Duration {
        if self.complete(command) {
            GLOW_STEP
        } else {
            TYPE_STEP
        }
    }

    fn glow(self) -> f32 {
        self.age.map_or(0.0, |age| {
            let half = GLOW_HALF_CYCLE.as_millis();
            let phase = (age.as_millis() % (half * 2)) as f32 / half as f32;
            1.0 - (1.0 - phase).abs()
        })
    }

    pub(crate) fn paint_enter_glow(
        self,
        frame: &mut canvas::Frame,
        at: Point,
        cell: Size,
        color: Color,
    ) {
        let glow = self.glow();
        if glow > 0.0 {
            frame.fill_rectangle(
                at,
                Size::new(cell.width * 5.0, cell.height),
                color.scale_alpha(0.24 * glow),
            );
        }
    }
}

pub(crate) fn paint_ghost(
    frame: &mut canvas::Frame,
    command: &str,
    position: Point,
    foreground: Color,
    font: Font,
    size: u8,
) {
    frame.fill_text(canvas::Text {
        content: command.into(),
        position,
        color: foreground.scale_alpha(0.72),
        size: Pixels(f32::from(size)),
        font: Font {
            style: Style::Italic,
            ..font
        },
        ..canvas::Text::default()
    });
}

impl Review<'_> {
    pub(super) fn position(&self, size: Size, cursor: Cursor, cell: Metrics) -> Option<Position> {
        self.inline
            .then(|| self.suggestion().position(size, cursor, cell))
            .flatten()
    }

    fn hint(&self) -> &'static str {
        approval_hint(self.editable, self.remaining_allowed)
    }

    fn suggestion(&self) -> Suggestion<'_> {
        Suggestion {
            command: self.command,
            destination: &self.destination,
            hint: self.hint(),
        }
    }

    pub(super) fn draw(
        &self,
        view: &TerminalView<'_>,
        renderer: &Renderer,
        size: Size,
        motion: MotionFrame,
    ) -> Geometry {
        let cell = Metrics::with_font(view.font_size, view.font);
        let position = view
            .projected_cursor()
            .and_then(|cursor| self.position(size, cursor, cell));
        self.suggestion()
            .draw(view, renderer, size, position, motion, 1.0)
    }
}

struct Suggestion<'a> {
    command: &'a str,
    destination: &'a str,
    hint: &'static str,
}

impl Suggestion<'_> {
    fn position(&self, size: Size, cursor: Cursor, cell: Metrics) -> Option<Position> {
        let inset = super::INSET;
        let ghost = cell.point(cursor.row, cursor.column.saturating_add(1));
        let line = self.hint;
        let columns = |text: &str| {
            text.chars()
                .map(|ch| if ch.is_ascii() { 1.0 } else { 2.0 })
                .sum::<f32>()
                * cell.width.max(cell.height * 0.56)
        };
        let bottom = size.height - cell.height;
        if !cursor.visible
            || self.command.contains(['\r', '\n', '\t'])
            || ghost.x + columns(self.command) + inset > size.width
            || columns(self.destination) + inset * 2.0 > size.width
            || columns(line) + inset * 2.0 > size.width
            || ghost.y + cell.height > size.height
        {
            return None;
        }
        let (hint_y, footer) = if ghost.y + cell.height * 2.0 <= bottom {
            (ghost.y + cell.height, bottom)
        } else if ghost.y + cell.height <= bottom {
            (ghost.y - cell.height, bottom)
        } else {
            (ghost.y - cell.height, ghost.y - 2.0 * cell.height)
        };
        (hint_y >= 0.0 && footer >= 0.0).then_some(Position {
            ghost,
            hint: Point::new(ghost.x.min(size.width - inset - columns(line)), hint_y),
            footer,
        })
    }

    fn draw(
        &self,
        view: &TerminalView<'_>,
        renderer: &Renderer,
        size: Size,
        position: Option<Position>,
        motion: MotionFrame,
        opacity: f32,
    ) -> Geometry {
        let cell = Metrics::with_font(view.font_size, view.font);
        let mut frame = canvas::Frame::new(renderer, size);
        let screen = &view.frame.screen;
        let foreground = view.appearance.foreground.color().scale_alpha(opacity);
        let background = default_background(screen, &view.appearance).scale_alpha(opacity);
        let (line, bottom) = if let Some(position) = position {
            let hint = self.hint;
            paint_ghost(
                &mut frame,
                motion.command(self.command),
                position.ghost,
                foreground,
                view.font,
                view.font_size,
            );
            frame.fill_rectangle(
                position.hint,
                Size::new(
                    (hint.len() as f32) * cell.width.max(cell.height * 0.56),
                    cell.height,
                ),
                background,
            );
            motion.paint_enter_glow(
                &mut frame,
                position.hint,
                Size::new(cell.width.max(cell.height * 0.56), cell.height),
                foreground,
            );
            frame.fill_text(canvas::Text {
                content: hint.into(),
                position: position.hint,
                color: foreground,
                size: Pixels(f32::from(view.font_size)),
                font: view.font,
                ..canvas::Text::default()
            });
            (self.destination, position.footer)
        } else {
            (
                "Agent waiting for review in the Activity window",
                size.height - cell.height,
            )
        };
        if bottom >= 0.0 {
            frame.with_clip(
                Rectangle::new(Point::new(0.0, bottom), Size::new(size.width, cell.height)),
                |frame| {
                    frame.fill_rectangle(
                        Point::new(0.0, bottom),
                        Size::new(size.width, cell.height),
                        background,
                    );
                    frame.fill_text(canvas::Text {
                        content: line.into(),
                        position: Point::new(super::INSET, bottom),
                        color: foreground,
                        size: Pixels(f32::from(view.font_size)),
                        font: view.font,
                        ..canvas::Text::default()
                    });
                },
            );
        }
        frame.into_geometry()
    }
}

pub(crate) fn approval_hint(editable: bool, remaining_allowed: bool) -> &'static str {
    match (editable, remaining_allowed) {
        (true, true) => "Enter/1 run  Tab edit  2 allow ordinary  Esc/3 dismiss",
        (false, true) => "Enter/1 run  2 allow ordinary  Esc/3 dismiss",
        (true, false) => "Enter/1 run  Tab edit  Esc/2 dismiss",
        (false, false) => "Enter/1 run  Esc/2 dismiss",
    }
}

#[cfg(test)]
mod tests {
    use iced::keyboard::{
        Location, Modifiers,
        key::{Code, Physical},
    };
    use iced::widget::canvas::Program as _;
    use iced::{Event, Font, mouse};

    use crate::terminal_theme::Appearance;

    use super::*;

    #[test]
    fn cursor_review_only_captures_visible_exact_approval_and_never_repeats() {
        let mut terminal = kortty_terminal::Terminal::new(80, 10);
        terminal.advance(b"\x1b[10;1H$ ");
        let frame = super::super::selection_test_frame(terminal.snapshot());
        let owner = window::Id::unique();
        let session = SessionId(1);
        let binding = Binding {
            owner,
            session,
            stream: 3,
            run: window::Id::unique(),
            epoch: 11,
            screen_revision: frame.search_revision,
            input_revision: 0,
        };
        let motion = RefCell::new(Motion::default());
        let mut view = TerminalView {
            owner,
            session,
            stream_generation: 3,
            frame: &frame,
            font_size: 14,
            font: Font::MONOSPACE,
            appearance: Appearance::default(),
            background_image: None,
            scale_factor: 1.0,
            background_transparency: 0,
            focused: true,
            reduced_motion: true,
            typeahead_enabled: false,
            plain_text_links: false,
            selection: None,
            projected: None,
            search: None,
            agent_cursor: Some(Surface {
                review: Some(Review {
                    binding,
                    command: "printf '%s' 'visible draft'",
                    destination: "/fixture".into(),
                    inline: true,
                    editable: true,
                    remaining_allowed: false,
                    prompt: None,
                }),
                departure: None,
                motion: &motion,
            }),
            inline_agent: None,
        };
        let bounds = Rectangle::with_size(Size::new(620.0, 190.0));
        let metrics = Metrics::with_font(view.font_size, view.font);
        let position = view
            .agent_cursor
            .as_ref()
            .and_then(|surface| surface.review.as_ref())
            .unwrap()
            .position(bounds.size(), frame.screen.cursor, metrics)
            .unwrap();
        assert_eq!(position.ghost, metrics.point(9, 3));
        assert!(position.hint.y < position.ghost.y);
        assert!(position.footer < position.hint.y);

        let press = |view: &TerminalView<'_>,
                     state: &mut super::super::State,
                     (key, code): (keyboard::Key, Code),
                     repeat,
                     modifiers,
                     bounds| {
            view.update(
                state,
                &Event::Keyboard(keyboard::Event::KeyPressed {
                    key: key.clone(),
                    modified_key: key,
                    physical_key: Physical::Code(code),
                    location: Location::Standard,
                    modifiers,
                    text: None,
                    repeat,
                }),
                bounds,
                mouse::Cursor::Unavailable,
            )
            .map(|action| action.into_inner())
        };
        let enter = || (keyboard::Key::Named(Named::Enter), Code::Enter);
        let mut state = super::super::State::default();
        let (message, _, status) = press(
            &view,
            &mut state,
            enter(),
            false,
            Modifiers::empty(),
            bounds,
        )
        .unwrap();
        assert_eq!(status, iced::event::Status::Captured);
        assert!(
            matches!(message, Some(crate::workspace::Message::AgentCursor(Message::Key(token, Key::Enter, Decision::ApproveOnce))) if token == binding)
        );
        let (message, _, status) =
            press(&view, &mut state, enter(), true, Modifiers::empty(), bounds).unwrap();
        assert!(message.is_none());
        assert_eq!(status, iced::event::Status::Captured);
        let (_, _, status) =
            press(&view, &mut state, enter(), false, Modifiers::SHIFT, bounds).unwrap();
        assert_eq!(status, iced::event::Status::Ignored);
        let too_narrow = Rectangle::with_size(Size::new(92.0, 190.0));
        let (_, _, status) = press(
            &view,
            &mut state,
            enter(),
            false,
            Modifiers::empty(),
            too_narrow,
        )
        .unwrap();
        assert_eq!(status, iced::event::Status::Ignored);
        view.reduced_motion = false;
        let (message, _, status) = press(
            &view,
            &mut state,
            enter(),
            false,
            Modifiers::empty(),
            bounds,
        )
        .unwrap();
        assert!(matches!(
            message,
            Some(crate::workspace::Message::AgentCursor(Message::Consumed(
                Key::Enter
            )))
        ));
        assert_eq!(status, iced::event::Status::Captured);
        let _ = view.update(
            &mut state,
            &Event::Window(window::Event::RedrawRequested(Instant::now())),
            bounds,
            mouse::Cursor::Unavailable,
        );
        let (message, _, _) =
            press(&view, &mut state, enter(), true, Modifiers::empty(), bounds).unwrap();
        assert!(message.is_none());
        let (message, _, _) = press(
            &view,
            &mut state,
            enter(),
            false,
            Modifiers::empty(),
            bounds,
        )
        .unwrap();
        assert!(
            matches!(message, Some(crate::workspace::Message::AgentCursor(Message::Key(token, Key::Enter, Decision::ApproveOnce))) if token == binding)
        );

        view.reduced_motion = true;
        let cases = [
            (false, "2", Code::Digit2, Key::Two, Some(Decision::Dismiss)),
            (false, "3", Code::Digit3, Key::Three, None),
            (
                true,
                "2",
                Code::Numpad2,
                Key::Two,
                Some(Decision::ApproveRemaining),
            ),
            (
                true,
                "3",
                Code::Numpad3,
                Key::Three,
                Some(Decision::Dismiss),
            ),
        ];
        for (remaining, digit, code, expected_key, expected_decision) in cases {
            view.agent_cursor
                .as_mut()
                .unwrap()
                .review
                .as_mut()
                .unwrap()
                .remaining_allowed = remaining;
            let (message, _, status) = press(
                &view,
                &mut state,
                (keyboard::Key::Character(digit.into()), code),
                false,
                Modifiers::empty(),
                bounds,
            )
            .unwrap();
            assert_eq!(status, iced::event::Status::Captured);
            if let Some(expected) = expected_decision {
                assert!(
                    matches!(message, Some(crate::workspace::Message::AgentCursor(Message::Key(token, key, decision))) if token == binding && key == expected_key && decision == expected)
                );
            } else {
                assert!(
                    matches!(message, Some(crate::workspace::Message::AgentCursor(Message::Consumed(key))) if key == expected_key)
                );
            }
        }

        view.agent_cursor
            .as_mut()
            .unwrap()
            .review
            .as_mut()
            .unwrap()
            .inline = false;
        let (_, _, status) = press(
            &view,
            &mut state,
            enter(),
            false,
            Modifiers::empty(),
            bounds,
        )
        .unwrap();
        assert_eq!(status, iced::event::Status::Ignored);
        let surface = view.agent_cursor.as_mut().unwrap();
        surface.review = None;
        surface.departure = Some(Departure {
            binding,
            fade: Fade::new(Instant::now()),
        });
        let (message, _, status) = press(
            &view,
            &mut state,
            enter(),
            false,
            Modifiers::empty(),
            bounds,
        )
        .unwrap();
        assert!(message.is_none());
        assert_eq!(status, iced::event::Status::Ignored);
    }

    #[test]
    fn cursor_motion_tracks_the_review_across_screen_updates_and_respects_reduced_motion() {
        let now = Instant::now();
        let mut review = Review {
            binding: Binding {
                owner: window::Id::unique(),
                session: SessionId(1),
                stream: 2,
                run: window::Id::unique(),
                epoch: 4,
                screen_revision: 1,
                input_revision: 1,
            },
            command: "écho 🦀",
            destination: "/fixture".into(),
            inline: true,
            editable: false,
            remaining_allowed: false,
            prompt: None,
        };
        let mut motion = Motion::default();
        assert_eq!(
            motion.advance(Some(&review), None, now, false),
            Some(now + TYPE_STEP)
        );
        assert_eq!(
            motion.frame(review.binding, false).command(review.command),
            ""
        );
        review.binding.screen_revision += 1;
        motion.advance(Some(&review), None, now + TYPE_STEP, false);
        assert_eq!(
            motion.frame(review.binding, false).command(review.command),
            "é"
        );
        motion.advance(Some(&review), None, now + GLOW_HALF_CYCLE, false);
        let settled = motion.frame(review.binding, false);
        assert_eq!(settled.command(review.command), review.command);
        assert_eq!(settled.interval(review.command), GLOW_STEP);
        assert_eq!(settled.glow(), 1.0);
        assert_eq!(
            motion.advance(Some(&review), None, now + GLOW_HALF_CYCLE, true),
            None
        );
        let still = motion.frame(review.binding, true);
        assert_eq!(still.command(review.command), review.command);
        assert_eq!(still.glow(), 0.0);
        motion.advance(Some(&review), None, now + GLOW_HALF_CYCLE, false);
        review.binding.epoch += 1;
        motion.advance(Some(&review), None, now + GLOW_HALF_CYCLE * 2, false);
        assert_eq!(
            motion.frame(review.binding, false).command(review.command),
            ""
        );
        let closed = now + GLOW_HALF_CYCLE * 2;
        let departure = Departure {
            binding: review.binding,
            fade: Fade::new(closed),
        };
        assert_eq!(
            motion.advance(None, Some(departure), closed, false),
            Some(closed + FADE_STEP)
        );
        let middle = closed + FADE_DURATION / 2;
        assert_eq!(departure.fade.opacity(middle), Some(0.5));
        assert_eq!(
            motion.advance(None, Some(departure), middle, false),
            Some(middle + FADE_STEP)
        );
        let (active, painted) = motion.departing(departure).unwrap();
        assert!(active.frame.command(&painted.command).is_empty());
        assert_eq!(
            motion.advance(None, Some(departure), closed + FADE_DURATION, false),
            None
        );
        assert!(motion.0.is_none());

        motion.advance(Some(&review), None, closed, false);
        let mut stale = departure;
        stale.binding.input_revision += 1;
        assert_eq!(motion.advance(None, Some(stale), middle, false), None);
        assert!(motion.0.is_none());
        motion.advance(Some(&review), None, closed, false);
        assert_eq!(motion.advance(None, Some(departure), middle, true), None);
        assert!(motion.0.is_none());
    }
}
