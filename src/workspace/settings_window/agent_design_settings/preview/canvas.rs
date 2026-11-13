use iced::widget::canvas::{self, Action, Geometry};
use iced::{Color, Event, Font, Pixels, Point, Rectangle, Renderer, Size, Theme, mouse};

use super::*;
use inline::{Line, Role, Segment, Style};

pub(super) struct Preview {
    pub model: Transcript,
    pub style: Option<Style>,
    pub token: Token,
    pub playing: bool,
    pub owner: window::Id,
}

#[derive(Default)]
pub(super) struct Clock {
    token: Option<Token>,
    started: Option<Instant>,
    now: Option<Instant>,
    published: bool,
}

impl canvas::Program<Desktop> for Preview {
    type State = Clock;

    fn update(
        &self,
        clock: &mut Clock,
        event: &Event,
        _: Rectangle,
        _: mouse::Cursor,
    ) -> Option<Action<Desktop>> {
        let Event::Window(window::Event::RedrawRequested(now)) = event else {
            return None;
        };
        if clock.token != Some(self.token) || !self.playing {
            clock.token = Some(self.token);
            clock.started = self.playing.then_some(*now);
            clock.published = false;
        }
        clock.now = Some(*now);
        if !self.playing {
            return None;
        }
        let started = *clock.started.get_or_insert(*now);
        let dwell = [1500, 1500, 2300, 2300, 1600]
            .get(usize::from(self.token.stage))
            .copied()?;
        if !clock.published && now.duration_since(started) >= Duration::from_millis(dwell) {
            clock.published = true;
            return Some(Action::publish(message(
                self.owner,
                super::Action::Tick(self.token),
            )));
        }
        Some(Action::request_redraw_at(*now + Duration::from_millis(150)))
    }

    fn draw(
        &self,
        clock: &Clock,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let palette = theme.palette();
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), palette.background);
        let cell = crate::terminal_view::cell_size(12, Font::MONOSPACE);
        let columns = ((bounds.width - 16.0) / cell.width).max(1.0) as usize;
        // This parser has no transport, actor, provider, or callback. It only formats fixture cells.
        let mut terminal = kortty_terminal::Terminal::new(columns.min(u16::MAX as usize) as u16, 4);
        terminal.advance(b"admin@web-01:~$ df -h /var\r\nFilesystem   Size Used Avail Use% Mounted on\r\n/dev/sda3     40G  37G  3.1G   93% /var\r\n");
        let snapshot = terminal.snapshot();
        let mut y = 6.0;
        for row in 0..3 {
            if let Some(cells) = snapshot.row(row) {
                let text: String = cells
                    .iter()
                    .filter(|cell| cell.width != 0)
                    .map(|cell| {
                        if cell.text.is_empty() {
                            " "
                        } else {
                            &cell.text
                        }
                    })
                    .collect();
                paint(
                    &mut frame,
                    &Line {
                        segments: vec![Segment {
                            text,
                            role: Role::Muted,
                        }],
                    },
                    y,
                    cell,
                    palette.text,
                    palette.primary,
                );
                y += cell.height;
            }
        }
        let top = y;
        if let Some(style) = self.style {
            let phase = clock.now.zip(clock.started).map_or(0, |(now, start)| {
                now.duration_since(start).as_millis() / 150
            });
            let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"][phase as usize % 10];
            let lines = inline::render(style, &self.model, columns, spinner);
            let available = ((bounds.height - y - cell.height) / cell.height).max(0.0) as usize;
            for line in lines.iter().take(available) {
                paint(&mut frame, line, y, cell, palette.text, palette.primary);
                y += cell.height;
            }
            if style == Style::Focus && !self.model.state.finished() {
                frame.fill_rectangle(
                    Point::ORIGIN,
                    Size::new(bounds.width, top),
                    palette.background.scale_alpha(0.58),
                );
                let bottom = (y + cell.height).min(bounds.height);
                frame.fill_rectangle(
                    Point::new(0.0, bottom),
                    Size::new(bounds.width, (bounds.height - bottom).max(0.0)),
                    palette.background.scale_alpha(0.58),
                );
                frame.fill_rectangle(
                    Point::new(10.0, top + 1.0),
                    Size::new(3.0, (y - top - 2.0).max(0.0)),
                    palette.primary,
                );
                frame.fill_rectangle(
                    Point::new(8.0, y - 1.0),
                    Size::new((bounds.width - 16.0).max(0.0), 2.0),
                    palette.primary.scale_alpha(0.55),
                );
            }
        }
        paint(
            &mut frame,
            &Line {
                segments: vec![Segment {
                    text: "admin@web-01:~$ ".into(),
                    role: Role::Accent,
                }],
            },
            y,
            cell,
            palette.text,
            palette.primary,
        );
        vec![frame.into_geometry()]
    }
}

fn paint(
    frame: &mut canvas::Frame,
    line: &Line,
    y: f32,
    cell: Size,
    foreground: Color,
    accent: Color,
) {
    let mut x = 8.0;
    for segment in &line.segments {
        let color = match segment.role {
            Role::Border | Role::Muted => foreground.scale_alpha(0.65),
            Role::Accent | Role::Key => accent,
            Role::Ok => Color::from_rgb8(81, 174, 100),
            Role::Warn => Color::from_rgb8(203, 155, 57),
            Role::Error => Color::from_rgb8(218, 91, 91),
            Role::Think => Color::from_rgb8(158, 132, 218),
            _ => foreground,
        };
        frame.fill_text(canvas::Text {
            content: segment.text.clone(),
            position: Point::new(x, y),
            color,
            size: Pixels(12.0),
            font: Font::MONOSPACE,
            ..canvas::Text::default()
        });
        x += segment
            .text
            .chars()
            .map(|ch| kortty_terminal::character_cell_width(ch).unwrap_or(0))
            .sum::<usize>() as f32
            * cell.width;
    }
}
