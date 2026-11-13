//! Non-interactive drawing and keyboard capture for an exact command review.

use iced::font::Style;
use iced::keyboard::{self, key::Named};
use iced::widget::canvas::{self, Geometry};
use iced::{Color, Pixels, Point, Rectangle, Renderer, Size, window};
use kortty_terminal::Cursor;

use super::{Metrics, TerminalView, default_background};
use crate::session::SessionId;

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

#[derive(Clone, Copy, Debug)]
pub enum Message {
    Key(Binding, Key),
    Edited(Binding, Result<(), &'static str>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Key {
    Enter,
    Tab,
    Escape,
    One,
    Two,
}

impl Key {
    pub(crate) fn from_key(key: &keyboard::Key) -> Option<Self> {
        match key {
            keyboard::Key::Named(Named::Enter) => Some(Self::Enter),
            keyboard::Key::Named(Named::Tab) => Some(Self::Tab),
            keyboard::Key::Named(Named::Escape) => Some(Self::Escape),
            keyboard::Key::Character(value) if value == "1" => Some(Self::One),
            keyboard::Key::Character(value) if value == "2" => Some(Self::Two),
            _ => None,
        }
    }

    pub(crate) fn mask(self) -> u8 {
        1 << self as u8
    }
}

pub(crate) struct Review<'a> {
    pub binding: Binding,
    pub command: &'a str,
    pub destination: String,
    pub inline: bool,
    pub editable: bool,
    pub prompt: Option<crate::session::ShortcutPrompt>,
}

pub(super) struct Position {
    ghost: Point,
    hint: Point,
    footer: f32,
}

impl Review<'_> {
    pub(super) fn position(&self, size: Size, cursor: Cursor, cell: Metrics) -> Option<Position> {
        let inset = super::INSET;
        let ghost = cell.point(cursor.row, cursor.column.saturating_add(1));
        let line = self.hint();
        let columns = |text: &str| {
            text.chars()
                .map(|ch| if ch.is_ascii() { 1.0 } else { 2.0 })
                .sum::<f32>()
                * cell.width.max(cell.height * 0.56)
        };
        let bottom = size.height - cell.height;
        if !self.inline
            || !cursor.visible
            || self.command.contains(['\r', '\n', '\t'])
            || ghost.x + columns(self.command) + inset > size.width
            || columns(&self.destination) + inset * 2.0 > size.width
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

    fn hint(&self) -> &'static str {
        if self.editable {
            "Enter run   Tab edit   Esc dismiss"
        } else {
            "Enter run   Esc dismiss"
        }
    }

    pub(super) fn draw(
        &self,
        view: &TerminalView<'_>,
        renderer: &Renderer,
        size: Size,
    ) -> Geometry {
        let cell = Metrics::with_font(view.font_size, view.font);
        let mut frame = canvas::Frame::new(renderer, size);
        let screen = &view.frame.screen;
        let foreground = view.appearance.foreground.color();
        let background = default_background(screen, &view.appearance);
        let position = view
            .projected_cursor()
            .and_then(|cursor| self.position(size, cursor, cell));
        let (line, bottom) = if let Some(position) = position {
            let hint = self.hint();
            frame.fill_text(canvas::Text {
                content: self.command.into(),
                position: position.ghost,
                color: Color {
                    a: 0.72,
                    ..foreground
                },
                size: Pixels(f32::from(view.font_size)),
                font: iced::Font {
                    style: Style::Italic,
                    ..view.font
                },
                ..canvas::Text::default()
            });
            frame.fill_rectangle(
                position.hint,
                Size::new(
                    (hint.len() as f32) * cell.width.max(cell.height * 0.56),
                    cell.height,
                ),
                background,
            );
            frame.fill_text(canvas::Text {
                content: hint.into(),
                position: position.hint,
                color: foreground,
                size: Pixels(f32::from(view.font_size)),
                font: view.font,
                ..canvas::Text::default()
            });
            (self.destination.as_str(), position.footer)
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
        let mut view = TerminalView {
            owner,
            session,
            frame: &frame,
            font_size: 14,
            font: Font::MONOSPACE,
            appearance: Appearance::default(),
            background_image: None,
            scale_factor: 1.0,
            background_transparency: 0,
            focused: true,
            reduced_motion: false,
            typeahead_enabled: false,
            plain_text_links: false,
            selection: None,
            projected: None,
            search: None,
            agent_cursor: Some(Review {
                binding,
                command: "printf '%s' 'visible draft'",
                destination: "/fixture".into(),
                inline: true,
                editable: true,
                prompt: None,
            }),
            inline_agent: None,
        };
        let bounds = Rectangle::with_size(Size::new(620.0, 190.0));
        let metrics = Metrics::with_font(view.font_size, view.font);
        let position = view
            .agent_cursor
            .as_ref()
            .unwrap()
            .position(bounds.size(), frame.screen.cursor, metrics)
            .unwrap();
        assert_eq!(position.ghost, metrics.point(9, 3));
        assert!(position.hint.y < position.ghost.y);
        assert!(position.footer < position.hint.y);

        let press = |view: &TerminalView<'_>, repeat, modifiers, bounds| {
            view.update(
                &mut super::super::State::default(),
                &Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Named(Named::Enter),
                    modified_key: keyboard::Key::Named(Named::Enter),
                    physical_key: Physical::Code(Code::Enter),
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
        let (message, _, status) = press(&view, false, Modifiers::empty(), bounds).unwrap();
        assert_eq!(status, iced::event::Status::Captured);
        assert!(
            matches!(message, Some(crate::workspace::Message::AgentCursor(Message::Key(token, Key::Enter))) if token == binding)
        );
        let (message, _, status) = press(&view, true, Modifiers::empty(), bounds).unwrap();
        assert!(message.is_none());
        assert_eq!(status, iced::event::Status::Captured);
        let (_, _, status) = press(&view, false, Modifiers::SHIFT, bounds).unwrap();
        assert_eq!(status, iced::event::Status::Ignored);
        let too_narrow = Rectangle::with_size(Size::new(92.0, 190.0));
        let (_, _, status) = press(&view, false, Modifiers::empty(), too_narrow).unwrap();
        assert_eq!(status, iced::event::Status::Ignored);
        view.agent_cursor.as_mut().unwrap().inline = false;
        let (_, _, status) = press(&view, false, Modifiers::empty(), bounds).unwrap();
        assert_eq!(status, iced::event::Status::Ignored);
    }
}
