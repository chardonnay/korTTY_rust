use std::time::{Duration, Instant};

use iced::widget::canvas::{self, Canvas};
use iced::widget::{Row, column, container, row, rule, space, text, tooltip};
use iced::{Color, Element, Font, Length, Pixels, Point, Rectangle, Renderer, Theme, font, mouse};
use jiff::{Timestamp, Zoned, tz::TimeZone};
use kortty_terminal::{CommandTimestamp, CommandTimestampStatus};

use super::{Appearance, Frame, INSET, Message, Metrics, default_background};

pub(crate) const TIMESTAMP_GUTTER_WIDTH: u16 = 88;
const WIDTH: f32 = TIMESTAMP_GUTTER_WIDTH as f32;
const SEPARATOR_WIDTH: f32 = 1.0;
const TEXT_LEFT: f32 = 6.0;
const GLYPH_GAP: f32 = 4.0;
const SEPARATOR: Color = Color::from_rgb8(68, 68, 68);

pub(crate) struct Gutter<'a> {
    pub(crate) frame: &'a Frame,
    pub(crate) font_size: u8,
    pub(crate) font: Font,
    pub(crate) appearance: Appearance,
}

pub(crate) fn with_timestamp_gutter<'a>(
    terminal: Element<'a, Message>,
    gutter: Option<Gutter<'a>>,
) -> Element<'a, Message> {
    let gutter = gutter.map_or_else(|| space::horizontal().width(0).into(), Gutter::view);
    // from_vec retains the zero-width slot so the terminal keeps its input state.
    Row::from_vec(vec![gutter, terminal])
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

impl<'a> Gutter<'a> {
    fn view(self) -> Element<'a, Message> {
        let screen = &self.frame.screen;
        let background = gutter_background(default_background(screen, &self.appearance));
        let foreground = if screen.reverse_video {
            self.appearance.background.color()
        } else {
            self.appearance.foreground.color()
        };
        let paint = Paint {
            foreground,
            light: brightness(background) >= 0.5,
            font: self.font,
            font_size: f32::from(self.font_size),
        };
        let pitch = Metrics::new(self.font_size).height;
        let mut rows = column![space::vertical().height(INSET)];
        let mut next_row = 0;
        let mut zone = None;
        for mark in &self.frame.command_timestamps {
            let Some(row) = self.frame.raw_display_row(mark.row) else {
                continue;
            };
            if row < next_row || row >= screen.rows {
                continue;
            }
            if row > next_row {
                rows = rows.push(space::vertical().height(pitch * f32::from(row - next_row)));
            }
            let zone = zone.get_or_insert_with(TimeZone::system);
            rows = rows.push(mark_view(mark, zone, paint, pitch));
            next_row = row + 1;
        }

        let rows = container(rows)
            .width(WIDTH - SEPARATOR_WIDTH)
            .height(Length::Fill)
            .clip(true);
        let separator = rule::vertical(SEPARATOR_WIDTH).style(|theme| rule::Style {
            color: SEPARATOR,
            ..rule::default(theme)
        });
        container(row![rows, separator].height(Length::Fill))
            .width(WIDTH)
            .height(Length::Fill)
            .style(move |_| container::Style::default().background(background))
            .into()
    }
}

fn mark_view(
    mark: &CommandTimestamp,
    zone: &TimeZone,
    paint: Paint,
    pitch: f32,
) -> Element<'static, Message> {
    let local = Timestamp::try_from(mark.time)
        .ok()
        .map(|time| time.to_zoned(zone.clone()));
    let elapsed = mark
        .previous
        .and_then(|previous| mark.time.duration_since(previous).ok());
    let duration = match mark.status.as_ref() {
        Some(CommandTimestampStatus::Finished { duration, .. }) => Some(compact(*duration, true)),
        _ => elapsed.map(|elapsed| compact(elapsed, false)),
    };
    let mut top = local
        .as_ref()
        .map_or_else(String::new, |date| date.strftime("%m/%d").to_string());
    if let Some(duration) = duration {
        if !top.is_empty() {
            top.push(' ');
        }
        top.push_str(&duration);
    }
    let bottom = local
        .as_ref()
        .map_or_else(String::new, |time| time.strftime("%H:%M:%S").to_string());
    let glyph = status_glyph(mark.status.as_ref(), paint);
    let canvas = Canvas::new(Mark {
        top,
        bottom,
        glyph,
        paint,
    })
    .width(Length::Fill)
    .height(pitch);
    let details = hover_details(local.as_ref(), mark.status.as_ref(), elapsed);
    tooltip(canvas, details, tooltip::Position::FollowCursor)
        .gap(10)
        .padding(8)
        .delay(Duration::ZERO)
        .style(|theme| {
            let background = theme.extended_palette().background.weakest.color;
            container::bordered_box(theme).background(alpha(background, 1.0))
        })
        .into()
}

#[derive(Clone, Copy)]
struct Paint {
    foreground: Color,
    light: bool,
    font: Font,
    font_size: f32,
}

struct Mark {
    top: String,
    bottom: String,
    glyph: Option<(&'static str, Color)>,
    paint: Paint,
}

impl canvas::Program<Message> for Mark {
    type State = ();

    fn draw(
        &self,
        _: &(),
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let Paint {
            foreground,
            font,
            font_size,
            ..
        } = self.paint;
        let height = bounds.height;
        let date_height = height * 0.44;
        let date_size = (font_size * 0.55).max(7.0).min(date_height);
        let time_size = (font_size * 0.75).max(8.0).min(height - date_height);
        let width = bounds.width;
        let time_size = time_size.min((width - 2.0 - TEXT_LEFT) / (8.0 * 0.61));
        fill_text(
            &mut frame,
            &self.top,
            Point::new(TEXT_LEFT, 0.0),
            date_size,
            font,
            alpha(foreground, 0.72 * 0.86),
        );
        fill_text(
            &mut frame,
            &self.bottom,
            Point::new(TEXT_LEFT, date_height),
            time_size,
            font,
            alpha(foreground, 0.86),
        );

        if let Some((glyph, color)) = self.glyph {
            let font = Font {
                weight: font::Weight::Bold,
                ..font
            };
            let right = width - 2.0;
            let after_time = TEXT_LEFT + time_size * 0.61 * 8.0 + GLYPH_GAP;
            let after_date =
                TEXT_LEFT + date_size * 0.61 * self.top.chars().count() as f32 + GLYPH_GAP;
            let (x, y, size) = if after_time + time_size <= right {
                (after_time, date_height, time_size)
            } else if after_date + date_size <= right {
                (after_date, 0.0, date_size)
            } else {
                ((right - time_size).max(TEXT_LEFT), date_height, time_size)
            };
            fill_text(&mut frame, glyph, Point::new(x, y), size, font, color);
        }
        vec![frame.into_geometry()]
    }
}

fn fill_text(
    frame: &mut canvas::Frame,
    value: &str,
    position: Point,
    size: f32,
    font: Font,
    color: Color,
) {
    if !value.is_empty() && size > 0.0 {
        frame.fill_text(canvas::Text {
            content: value.to_owned(),
            position,
            size: Pixels(size),
            line_height: Pixels(size).into(),
            color,
            font,
            ..canvas::Text::default()
        });
    }
}

fn hover_details(
    local: Option<&Zoned>,
    status: Option<&CommandTimestampStatus>,
    elapsed: Option<Duration>,
) -> Element<'static, Message> {
    let mut details = column![].spacing(4);
    if let Some(local) = local {
        details = details.push(text(local.strftime("%A, %B %e, %Y").to_string()).size(14));
        details = details.push(text(local.strftime("%H:%M:%S").to_string()).size(13));
    }
    match status {
        Some(CommandTimestampStatus::Running { since }) => {
            details = details.push(Canvas::new(Running(*since)).width(180).height(16));
        }
        Some(CommandTimestampStatus::Finished {
            exit_code,
            duration,
        }) => {
            details = details.push(text(format!("Runtime: {}", verbose(*duration, true))).size(13));
            if let Some(exit) = exit_code {
                details = details.push(text(format!("Exit status: {exit}")).size(13));
            }
        }
        None => {
            if let Some(elapsed) = elapsed {
                details =
                    details.push(text(format!("Elapsed: {}", verbose(elapsed, false))).size(13));
            }
        }
    }
    details.into()
}

struct Running(Instant);

impl canvas::Program<Message> for Running {
    type State = ();

    fn draw(
        &self,
        _: &(),
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let label = format!("Running for {}", verbose(self.0.elapsed(), true));
        fill_text(
            &mut frame,
            &label,
            Point::ORIGIN,
            13.0,
            Font::DEFAULT,
            theme.palette().text,
        );
        vec![frame.into_geometry()]
    }
}

fn compact(duration: Duration, runtime: bool) -> String {
    let seconds = duration.as_secs();
    if runtime && seconds == 0 {
        return "<1s".into();
    }
    let prefix = if runtime { "" } else { "+" };
    match seconds {
        0..60 => format!("{prefix}{seconds}s"),
        60..3600 => format!("{prefix}{}:{:02}", seconds / 60, seconds % 60),
        _ => format!(
            "{prefix}{}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        ),
    }
}

fn verbose(duration: Duration, runtime: bool) -> String {
    let seconds = duration.as_secs();
    match seconds {
        0 if runtime => format!("{} ms", duration.as_millis()),
        0..60 => format!("{seconds} sec"),
        60..3600 => format!("{} min {} sec", seconds / 60, seconds % 60),
        _ => format!(
            "{} h {} min {} sec",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        ),
    }
}

fn status_glyph(
    status: Option<&CommandTimestampStatus>,
    paint: Paint,
) -> Option<(&'static str, Color)> {
    match status? {
        CommandTimestampStatus::Running { .. } => Some(("…", alpha(paint.foreground, 0.86))),
        CommandTimestampStatus::Finished {
            exit_code: Some(0), ..
        } => Some((
            "✓",
            if paint.light {
                Color::from_rgb8(26, 127, 55)
            } else {
                Color::from_rgb8(63, 185, 80)
            },
        )),
        CommandTimestampStatus::Finished {
            exit_code: Some(_), ..
        } => Some((
            "✗",
            if paint.light {
                Color::from_rgb8(207, 34, 46)
            } else {
                Color::from_rgb8(255, 107, 97)
            },
        )),
        CommandTimestampStatus::Finished {
            exit_code: None, ..
        } => None,
    }
}

fn alpha(color: Color, a: f32) -> Color {
    Color { a, ..color }
}

fn brightness(color: Color) -> f32 {
    color.r.max(color.g).max(color.b)
}

fn gutter_background(color: Color) -> Color {
    let factor = if brightness(color) < 0.5 { 1.15 } else { 0.92 };
    Color::from_rgb(
        (color.r * factor).min(1.0),
        (color.g * factor).min(1.0),
        (color.b * factor).min(1.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_runtime_and_elapsed_formats_follow_the_reference() {
        assert_eq!(compact(Duration::from_millis(350), true), "<1s");
        assert_eq!(compact(Duration::from_millis(350), false), "+0s");
        assert_eq!(compact(Duration::from_secs(65), false), "+1:05");
        assert_eq!(compact(Duration::from_secs(3723), true), "1:02:03");
        assert_eq!(verbose(Duration::from_millis(350), true), "350 ms");
    }

    #[test]
    fn gutter_is_a_separate_column_and_the_terminal_uses_only_its_own_bounds() {
        use crate::session::{GridSize, Session, SessionId};
        use iced::advanced::{Layout, Shell, clipboard, layout, widget::Tree};
        use iced::{Event, Size, window};
        use kortty_terminal::TerminalProfile;

        let session = SessionId(1);
        let owner = window::Id::unique();
        let frame = Session::ssh(
            None,
            GridSize::default(),
            TerminalProfile::Xterm,
            true,
            None,
        )
        .unwrap()
        .frame();
        let appearance = Appearance::default();
        let size = Size::new(480.0, 220.0);
        let viewport = Rectangle::with_size(size);
        let renderer = Renderer::new(Font::DEFAULT, 14.0.into());
        for visible in [false, true] {
            let terminal = super::super::view(super::super::TerminalView {
                session,
                frame: &frame,
                font_size: 14,
                font: Font::MONOSPACE,
                appearance,
                background_image: None,
                scale_factor: 2.0,
                background_transparency: 0,
                focused: true,
                reduced_motion: false,
                typeahead_enabled: false,
                owner,
                plain_text_links: false,
                selection: None,
                projected: None,
                search: None,
                agent_cursor: None,
                inline_agent: None,
            });
            let mut view = with_timestamp_gutter(
                terminal,
                visible.then_some(Gutter {
                    frame: &frame,
                    font_size: 14,
                    font: Font::MONOSPACE,
                    appearance,
                }),
            );
            let mut tree = Tree::new(view.as_widget());
            let node = view.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, size),
            );
            let layout = Layout::new(&node);
            let gutter_width = if visible { WIDTH } else { 0.0 };
            let terminal_bounds = layout.children().nth(1).unwrap().bounds();
            assert_eq!(terminal_bounds.x, gutter_width);
            assert_eq!(terminal_bounds.width, size.width - gutter_width);
            let mut messages = Vec::new();
            view.as_widget_mut().update(
                &mut tree,
                &Event::Window(window::Event::RedrawRequested(Instant::now())),
                layout,
                mouse::Cursor::Unavailable,
                &renderer,
                &mut clipboard::Null,
                &mut Shell::new(&mut messages),
                &viewport,
            );
            let expected = Metrics::new(14).grid(terminal_bounds.size(), 2.0);
            assert!(
                matches!(messages.as_slice(), [Message::PaneResized(id, grid)] if *id == session && *grid == expected)
            );

            if visible {
                let left_press = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
                for (x, terminal_receives_press) in
                    [(WIDTH - 1.0, false), (WIDTH + INSET + 1.0, true)]
                {
                    messages.clear();
                    view.as_widget_mut().update(
                        &mut tree,
                        &left_press,
                        layout,
                        mouse::Cursor::Available(Point::new(x, INSET + 1.0)),
                        &renderer,
                        &mut clipboard::Null,
                        &mut Shell::new(&mut messages),
                        &viewport,
                    );
                    assert_eq!(
                        matches!(messages.as_slice(), [Message::Selection(id, None)] if *id == session),
                        terminal_receives_press
                    );
                }
            }
        }
    }
}
