use std::time::{Duration, Instant};

use iced::mouse::{self, Button};
use iced::widget::canvas::{self, Action, Frame as CanvasFrame, Geometry, Path};
use iced::widget::{Canvas, Stack, container};
use iced::{Color, Element, Event, Length, Point, Rectangle, Renderer, Size, Theme, window};
use kortty_terminal::SearchMatch;

use super::{
    Metrics, SEARCH_MATCH, TerminalView, default_background, wheel_scroll_lines, with_transparency,
};
use crate::session::SessionId;
use crate::workspace::Message;

const WIDTH: f32 = 14.0;
const ARROW_HEIGHT: f32 = 12.0;
const MIN_THUMB_HEIGHT: f32 = 32.0;
const TRACK_STEP: i32 = 10;
const REPEAT_INTERVAL: Duration = Duration::from_millis(200);

pub(crate) fn with_scrollback<'a>(
    content: Element<'a, Message>,
    scrollbar: Option<Scrollbar<'a>>,
) -> Element<'a, Message> {
    let inset = if scrollbar.is_some() { WIDTH } else { 0.0 };
    let mut layout = Stack::new().push(container(content).padding(iced::padding::right(inset)));
    if let Some(scrollbar) = scrollbar {
        // Give a captured thumb drag priority over mouse reporting in the terminal.
        layout = layout.push(
            container(Canvas::new(scrollbar).width(WIDTH).height(Length::Fill))
                .align_x(iced::alignment::Horizontal::Right)
                .width(Length::Fill)
                .height(Length::Fill),
        );
    }
    layout.into()
}

pub(crate) struct Scrollbar<'a> {
    session: SessionId,
    history: usize,
    offset: usize,
    rows: u16,
    cell_height: f32,
    background: Color,
    foreground: Color,
    matches: &'a [SearchMatch],
    coordinates: Option<&'a kortty_terminal::ScrollbackSnapshot>,
}

impl<'a> Scrollbar<'a> {
    pub(crate) fn new(terminal: &TerminalView<'a>) -> Self {
        let frame = terminal.frame;
        Self {
            session: terminal.session,
            history: frame.screen.scrollback_lines,
            offset: frame.scroll_offset,
            rows: frame.screen.rows,
            cell_height: Metrics::with_font(terminal.font_size, terminal.font).height,
            background: with_transparency(
                default_background(&frame.screen, &terminal.appearance),
                terminal.background_transparency,
            ),
            foreground: if frame.screen.reverse_video {
                terminal.appearance.background.color()
            } else {
                terminal.appearance.foreground.color()
            },
            matches: terminal.search.map_or(&[], |search| search.matches),
            coordinates: frame.scrollback.as_ref(),
        }
    }

    fn geometry(&self, height: f32) -> ScrollbarGeometry {
        ScrollbarGeometry::new(height, self.rows, self.history, self.offset)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    OlderArrow,
    OlderTrack,
    Thumb,
    NewerTrack,
    NewerArrow,
}

impl Part {
    fn step(self) -> i32 {
        match self {
            Self::OlderArrow => 1,
            Self::OlderTrack => TRACK_STEP,
            Self::Thumb => 0,
            Self::NewerTrack => -TRACK_STEP,
            Self::NewerArrow => -1,
        }
    }
}

struct ScrollbarGeometry {
    height: f32,
    track_top: f32,
    track_height: f32,
    thumb_top: f32,
    thumb_height: f32,
    history: usize,
}

impl ScrollbarGeometry {
    fn new(height: f32, rows: u16, history: usize, offset: usize) -> Self {
        let track_top = ARROW_HEIGHT.min(height / 2.0);
        let track_height = height - 2.0 * track_top;
        let (thumb_top, thumb_height) = if history == 0 {
            (track_top, 0.0)
        } else {
            let visible = f64::from(rows) / (history as f64 + f64::from(rows));
            let min = MIN_THUMB_HEIGHT.min(track_height / 2.0);
            let thumb_height = (f64::from(track_height) * visible) as f32;
            let thumb_height = thumb_height.clamp(min, track_height);
            let from_top = (history - offset.min(history)) as f64 / history as f64;
            let top = track_top + (f64::from(track_height - thumb_height) * from_top) as f32;
            (top, thumb_height)
        };
        Self {
            height,
            track_top,
            track_height,
            thumb_top,
            thumb_height,
            history,
        }
    }

    fn part(&self, y: f32) -> Option<Part> {
        if self.history == 0 || !(0.0..self.height).contains(&y) {
            return None;
        }
        Some(if y < self.track_top {
            Part::OlderArrow
        } else if y >= self.height - self.track_top {
            Part::NewerArrow
        } else if y < self.thumb_top {
            Part::OlderTrack
        } else if y < self.thumb_top + self.thumb_height {
            Part::Thumb
        } else {
            Part::NewerTrack
        })
    }

    fn drag_offset(&self, y: f32, grip: f32) -> Option<usize> {
        let travel = self.track_height - self.thumb_height;
        if self.history == 0 || travel <= 0.0 {
            return None;
        }
        let top = y - self.track_top - grip * self.thumb_height;
        let from_top = (f64::from(top) / f64::from(travel)).clamp(0.0, 1.0);
        let row = (from_top * self.history as f64).round() as usize;
        Some(self.history - row.min(self.history))
    }
}

#[derive(Default)]
pub(crate) struct State {
    session: Option<SessionId>,
    hover: Option<Part>,
    press: Option<Press>,
}

enum Press {
    Drag {
        grip: f32,
        last_anchor: Option<kortty_terminal::ScrollbackAnchor>,
    },
    Repeat {
        part: Part,
        track_y: Option<f32>,
        next: Option<Instant>,
    },
}

impl canvas::Program<Message> for Scrollbar<'_> {
    type State = State;

    fn update(
        &self,
        state: &mut State,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<Action<Message>> {
        if state.session != Some(self.session) {
            *state = State {
                session: Some(self.session),
                ..State::default()
            };
        }
        let geometry = self.geometry(bounds.height);
        let inside = cursor.position_in(bounds).map(|point| point.y);
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(Button::Left)) => {
                let y = inside?;
                let Some(part) = geometry.part(y) else {
                    return Some(Action::capture());
                };
                if part == Part::Thumb {
                    state.press = Some(Press::Drag {
                        grip: ((y - geometry.thumb_top) / geometry.thumb_height).clamp(0.0, 1.0),
                        last_anchor: self
                            .coordinates
                            .and_then(|coordinates| coordinates.anchor(self.offset)),
                    });
                    return Some(Action::request_redraw().and_capture());
                }
                state.press = Some(Press::Repeat {
                    part,
                    track_y: matches!(part, Part::OlderTrack | Part::NewerTrack).then_some(y),
                    next: Some(Instant::now() + REPEAT_INTERVAL),
                });
                Some(Action::publish(Message::Scroll(self.session, part.step())).and_capture())
            }
            Event::Mouse(mouse::Event::ButtonPressed(_)) if inside.is_some() => {
                Some(Action::capture())
            }
            Event::Mouse(mouse::Event::CursorMoved { .. })
            | Event::Mouse(mouse::Event::CursorLeft) => {
                let hover = inside.and_then(|y| geometry.part(y));
                let changed = state.hover != hover;
                state.hover = hover;
                if let Some(Press::Drag { grip, last_anchor }) = &mut state.press {
                    let offset = cursor
                        .position_from(bounds.position())
                        .and_then(|position| geometry.drag_offset(position.y, *grip));
                    if let Some(offset) = offset
                        && self.coordinates.is_some_and(|coordinates| {
                            coordinates.offset(last_anchor.as_ref()) != offset
                        })
                    {
                        let anchor = self
                            .coordinates
                            .and_then(|coordinates| coordinates.anchor(offset));
                        last_anchor.clone_from(&anchor);
                        return Some(
                            Action::publish(Message::ScrollTo(self.session, anchor)).and_capture(),
                        );
                    }
                    return Some(Action::capture());
                }
                changed.then(Action::request_redraw)
            }
            Event::Mouse(mouse::Event::ButtonReleased(Button::Left)) => state
                .press
                .take()
                .map(|_| Action::request_redraw().and_capture()),
            Event::Mouse(mouse::Event::WheelScrolled { delta }) if inside.is_some() => {
                let lines = wheel_scroll_lines(*delta, self.cell_height);
                Some(if lines == 0 || self.history == 0 {
                    Action::capture()
                } else {
                    Action::publish(Message::Scroll(self.session, lines)).and_capture()
                })
            }
            Event::Window(window::Event::Unfocused) => {
                let redraw = state.press.take().is_some() || state.hover.is_some();
                state.hover = None;
                redraw.then(Action::request_redraw)
            }
            Event::Window(window::Event::RedrawRequested(now)) => {
                let Some(Press::Repeat {
                    part,
                    track_y,
                    next,
                }) = &mut state.press
                else {
                    return None;
                };
                let deadline = (*next)?;
                if *now < deadline {
                    return Some(Action::request_redraw_at(deadline));
                }
                if track_y.or(inside).and_then(|y| geometry.part(y)) != Some(*part) {
                    *next = None;
                    return None;
                }
                *next = Some(*now + REPEAT_INTERVAL);
                Some(Action::publish(Message::Scroll(self.session, part.step())))
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        state: &State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let geometry = self.geometry(bounds.height);
        let mut frame = CanvasFrame::new(renderer, bounds.size());
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), self.background);
        let track = scrollbar_track(self.background);
        fill_rounded(
            &mut frame,
            bounds.width,
            geometry.track_top,
            geometry.track_height,
            track,
        );

        let total_rows = self.history.saturating_add(usize::from(self.rows));
        let mut previous_y = None;
        if total_rows > 0 && geometry.track_height >= 2.0 {
            for hit in self.matches {
                let fraction = hit.start.row.min(total_rows - 1) as f64 / total_rows as f64;
                let y = (fraction * f64::from(geometry.track_height - 2.0)).round() as f32;
                if previous_y != Some(y) {
                    previous_y = Some(y);
                    frame.fill_rectangle(
                        Point::new(0.0, geometry.track_top + y),
                        Size::new(bounds.width, 2.0),
                        SEARCH_MATCH,
                    );
                }
            }
        }

        let hover = cursor
            .position_in(bounds)
            .and_then(|point| geometry.part(point.y));
        if geometry.thumb_height > 0.0 {
            let shade = match (&state.press, hover) {
                (Some(Press::Drag { .. }), _) => 0x70,
                (_, Some(Part::Thumb)) => 0xa0,
                _ => 0x88,
            };
            fill_rounded(
                &mut frame,
                bounds.width,
                geometry.thumb_top,
                geometry.thumb_height,
                Color::from_rgb8(shade, shade, shade),
            );
        }

        let top = geometry.track_top / 2.0;
        let center = bounds.width / 2.0;
        for (older, y) in [(true, top), (false, bounds.height - top)] {
            if top < 2.0 || center < 3.0 {
                break;
            }
            let part = if older {
                Part::OlderArrow
            } else {
                Part::NewerArrow
            };
            let mut color = self.foreground;
            color.a = if hover == Some(part) {
                1.0
            } else if self.history == 0 {
                0.4
            } else {
                0.7
            };
            let direction = if older { -1.0 } else { 1.0 };
            let arrow = Path::new(|path| {
                path.move_to(Point::new(center, y + direction * 2.0));
                path.line_to(Point::new(center - 3.0, y - direction * 2.0));
                path.line_to(Point::new(center + 3.0, y - direction * 2.0));
                path.close();
            });
            frame.fill(&arrow, color);
        }
        vec![frame.into_geometry()]
    }
}

fn fill_rounded(frame: &mut CanvasFrame, width: f32, top: f32, height: f32, color: Color) {
    if width > 0.0 && height > 0.0 {
        frame.fill(
            &Path::rounded_rectangle(
                Point::new(0.0, top),
                Size::new(width, height),
                (width.min(height).min(8.0) / 2.0).into(),
            ),
            color,
        );
    }
}

fn scrollbar_track(background: Color) -> Color {
    const LIGHTEN: f32 = 24.0 / 255.0;
    Color::from_rgba(
        (background.r + LIGHTEN).min(1.0),
        (background.g + LIGHTEN).min(1.0),
        (background.b + LIGHTEN).min(1.0),
        background.a.max(0.35),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drag_compares_the_held_row_after_history_advances() {
        use iced::widget::canvas::Program as _;
        use kortty_terminal::Terminal;

        let mut terminal = Terminal::new(12, 3);
        terminal.advance(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix\r\nseven");
        let before = terminal.scrollback_snapshot();
        let anchor = before.anchor(3);
        terminal.advance(b"\r\neight\r\nnine\r\nten");
        let screen = terminal.snapshot();
        let after = terminal.scrollback_snapshot();
        let session = SessionId(123);
        let scrollbar = Scrollbar {
            session,
            history: screen.scrollback_lines,
            offset: terminal.scrollback_offset(anchor.as_ref()),
            rows: screen.rows,
            cell_height: 16.0,
            background: Color::BLACK,
            foreground: Color::WHITE,
            matches: &[],
            coordinates: Some(&after),
        };
        let bounds = Rectangle::new(Point::ORIGIN, Size::new(WIDTH, 500.0));
        let desired =
            ScrollbarGeometry::new(bounds.height, screen.rows, screen.scrollback_lines, 3);
        let point = Point::new(WIDTH / 2.0, desired.thumb_top + desired.thumb_height / 2.0);
        let mut state = State {
            session: Some(session),
            press: Some(Press::Drag {
                grip: 0.5,
                last_anchor: anchor,
            }),
            ..State::default()
        };
        let action = scrollbar
            .update(
                &mut state,
                &Event::Mouse(mouse::Event::CursorMoved { position: point }),
                bounds,
                mouse::Cursor::Available(point),
            )
            .unwrap();
        let (message, _, status) = action.into_inner();
        let Some(Message::ScrollTo(id, anchor)) = message else {
            panic!("drag did not publish the new row");
        };
        assert_eq!(id, session);
        assert_eq!(terminal.scrollback_offset(anchor.as_ref()), 3);
        assert_eq!(status, iced::event::Status::Captured);
    }

    #[test]
    fn thumb_tracks_the_viewport_and_drag_reaches_both_ends() {
        let history = 1_000_000;
        let oldest = ScrollbarGeometry::new(500.0, 40, history, history);
        let newest = ScrollbarGeometry::new(500.0, 40, history, 0);
        let middle = ScrollbarGeometry::new(500.0, 40, history, history / 2);
        assert_eq!(oldest.thumb_top, oldest.track_top);
        assert_eq!(
            newest.thumb_top + newest.thumb_height,
            newest.height - newest.track_top
        );
        assert_eq!(middle.thumb_height, MIN_THUMB_HEIGHT);
        assert_eq!(
            middle.drag_offset(middle.thumb_top + middle.thumb_height / 2.0, 0.5),
            Some(history / 2)
        );
        assert_eq!(middle.drag_offset(-10.0, 0.5), Some(history));
        assert_eq!(middle.drag_offset(510.0, 0.5), Some(0));
    }

    #[test]
    fn track_and_arrows_have_the_reference_increments() {
        let geometry = ScrollbarGeometry::new(200.0, 40, 120, 60);
        assert_eq!(geometry.part(0.0).map(Part::step), Some(1));
        assert_eq!(geometry.part(20.0).map(Part::step), Some(10));
        assert_eq!(geometry.part(100.0), Some(Part::Thumb));
        assert_eq!(geometry.part(180.0).map(Part::step), Some(-10));
        assert_eq!(geometry.part(199.0).map(Part::step), Some(-1));
        let empty = ScrollbarGeometry::new(200.0, 40, 0, 0);
        assert_eq!(empty.thumb_height, 0.0);
        assert_eq!(empty.part(100.0), None);
        assert_eq!(empty.drag_offset(100.0, 0.5), None);
    }
}
