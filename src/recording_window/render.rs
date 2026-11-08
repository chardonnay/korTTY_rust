use std::sync::Mutex;

use iced::advanced::graphics::Viewport;
use iced::advanced::graphics::geometry::Renderer as _;
use iced::advanced::renderer::Renderer as _;
use iced::font::{Style as FontStyle, Weight};
use iced::widget::canvas::{self, Cache};
use iced::widget::{Canvas, scrollable};
use iced::{Color, Element, Font, Length, Pixels, Point, Rectangle, Renderer, Size, Theme, mouse};
use kortty_recording::RecordedScreen;
use kortty_terminal::{Color as TerminalColor, CursorShape, Style};

use super::Message;

const FONT_SIZE: f32 = 14.0;
const CELL_WIDTH: f32 = FONT_SIZE * 0.61;
const CELL_HEIGHT: f32 = 18.0;
const INSET: f32 = 5.0;
const BACKGROUND: Color = Color::from_rgb(0.055, 0.065, 0.082);
const FOREGROUND: Color = Color::from_rgb(0.86, 0.89, 0.93);
pub(super) const MAX_IMAGE_PIXELS: u64 = 16 * 1024 * 1024;

pub(super) fn view<'a>(screen: &'a RecordedScreen, cache: &'a Cache) -> Element<'a, Message> {
    scrollable(
        Canvas::new(Grid { screen, cache })
            .width(grid_size(screen).width)
            .height(grid_size(screen).height),
    )
    .direction(scrollable::Direction::Both {
        horizontal: scrollable::Scrollbar::default(),
        vertical: scrollable::Scrollbar::default(),
    })
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// Rasterizes the full replay grid, independently of its current window or scroll position.
pub(crate) fn png(screen: &RecordedScreen) -> Result<Vec<u8>, String> {
    static RASTERIZER: Mutex<()> = Mutex::new(());

    let size = grid_size(screen);
    let width = size.width.ceil() as u32;
    let height = size.height.ceil() as u32;
    if u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS {
        return Err("The terminal screen exceeds the PNG size limit.".into());
    }
    let dimensions =
        tiny_skia::IntSize::from_wh(width, height).ok_or("Invalid terminal image dimensions.")?;
    // Only image workers take this lock, to bound the simultaneous offscreen allocation.
    let _rendering = RASTERIZER.lock().unwrap_or_else(|error| error.into_inner());
    let viewport = Viewport::with_physical_size(Size::new(width, height), 1.0);
    let mut renderer = iced_tiny_skia::Renderer::new(Font::MONOSPACE, Pixels(FONT_SIZE));
    renderer.reset(Rectangle::with_size(viewport.logical_size()));
    let mut frame = canvas::Frame::new(&renderer, viewport.logical_size());
    paint(screen, &mut frame);
    renderer.draw_geometry(frame.into_geometry());
    let pixels =
        iced_tiny_skia::window::compositor::screenshot(&mut renderer, &viewport, BACKGROUND);
    tiny_skia::Pixmap::from_vec(pixels, dimensions)
        .ok_or("Could not allocate the terminal PNG.")?
        .encode_png()
        .map_err(|error| format!("Could not encode the terminal PNG ({error})."))
}

fn grid_size(screen: &RecordedScreen) -> Size {
    Size::new(
        f32::from(screen.columns) * CELL_WIDTH + INSET * 2.0,
        f32::from(screen.rows) * CELL_HEIGHT + INSET * 2.0,
    )
}

struct Grid<'a> {
    screen: &'a RecordedScreen,
    cache: &'a Cache,
}

impl canvas::Program<Message> for Grid<'_> {
    type State = ();

    fn draw(
        &self,
        _: &(),
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        vec![
            self.cache
                .draw(renderer, bounds.size(), |frame| paint(self.screen, frame)),
        ]
    }
}

fn paint<R: iced::advanced::graphics::geometry::Renderer>(
    screen: &RecordedScreen,
    frame: &mut canvas::Frame<R>,
) {
    let default = if screen.reverse_video {
        FOREGROUND
    } else {
        BACKGROUND
    };
    frame.fill_rectangle(Point::ORIGIN, frame.size(), default);
    for row in 0..screen.rows {
        let Some(cells) = screen.row(row) else { break };
        let cursor = |column| {
            screen.cursor.visible
                && screen.cursor.row == row
                && usize::from(screen.cursor.column) == column
        };
        for (column, cell) in cells.iter().enumerate() {
            let (foreground, background) = colors(cell.style, screen.reverse_video);
            let background = if cursor(column) && screen.cursor_shape == CursorShape::Block {
                foreground
            } else {
                background
            };
            if background != default {
                frame.fill_rectangle(
                    point(row, column as u16),
                    Size::new(CELL_WIDTH, CELL_HEIGHT),
                    background,
                );
            }
        }
        for (column, cell) in cells.iter().enumerate() {
            let position = point(row, column as u16);
            let (foreground, background) = colors(cell.style, screen.reverse_video);
            let foreground = if cursor(column) && screen.cursor_shape == CursorShape::Block {
                background
            } else {
                foreground
            };
            if cell.width != 0 && !cell.style.hidden && !cell.text.is_empty() {
                paint_cell(
                    frame, position, foreground, cell.style, cell.width, &cell.text,
                );
            }
            if cursor(column) {
                let cursor = match screen.cursor_shape {
                    CursorShape::Bar => Some((position, Size::new(2.0, CELL_HEIGHT))),
                    CursorShape::Underline => Some((
                        Point::new(position.x, position.y + CELL_HEIGHT - 2.0),
                        Size::new(CELL_WIDTH, 2.0),
                    )),
                    CursorShape::Block => None,
                };
                if let Some((position, size)) = cursor {
                    frame.fill_rectangle(position, size, foreground);
                }
            }
        }
    }
}

fn paint_cell<R: iced::advanced::graphics::geometry::Renderer>(
    frame: &mut canvas::Frame<R>,
    position: Point,
    mut foreground: Color,
    style: Style,
    width: u8,
    text: &str,
) {
    if style.dim {
        foreground.a = 0.6;
    }
    frame.fill_text(canvas::Text {
        content: text.to_owned(),
        position,
        size: Pixels(FONT_SIZE),
        color: foreground,
        font: Font {
            weight: if style.bold {
                Weight::Bold
            } else {
                Weight::Normal
            },
            style: if style.italic {
                FontStyle::Italic
            } else {
                FontStyle::Normal
            },
            ..Font::MONOSPACE
        },
        ..canvas::Text::default()
    });
    let span = Size::new(CELL_WIDTH * f32::from(width), 1.0);
    if style.underline {
        frame.fill_rectangle(
            Point::new(position.x, position.y + CELL_HEIGHT - 3.0),
            span,
            foreground,
        );
    }
    if style.strikethrough {
        frame.fill_rectangle(
            Point::new(position.x, position.y + CELL_HEIGHT / 2.0),
            span,
            foreground,
        );
    }
}

fn point(row: u16, column: u16) -> Point {
    Point::new(
        INSET + f32::from(column) * CELL_WIDTH,
        INSET + f32::from(row) * CELL_HEIGHT,
    )
}

fn colors(style: Style, reverse_video: bool) -> (Color, Color) {
    let foreground = color(style.foreground, FOREGROUND);
    let background = color(style.background, BACKGROUND);
    if style.inverse ^ reverse_video {
        (background, foreground)
    } else {
        (foreground, background)
    }
}

fn color(value: TerminalColor, default: Color) -> Color {
    match value {
        TerminalColor::Default => default,
        TerminalColor::Rgb(red, green, blue) => Color::from_rgb8(red, green, blue),
        TerminalColor::Indexed(index) => {
            let [red, green, blue] = match index {
                0..=15 => ANSI[usize::from(index)],
                16..=231 => {
                    let index = index - 16;
                    let steps = [0, 95, 135, 175, 215, 255];
                    [
                        steps[usize::from(index / 36)],
                        steps[usize::from(index / 6 % 6)],
                        steps[usize::from(index % 6)],
                    ]
                }
                _ => [8 + (index - 232) * 10; 3],
            };
            Color::from_rgb8(red, green, blue)
        }
    }
}

const ANSI: [[u8; 3]; 16] = [
    [0, 0, 0],
    [205, 0, 0],
    [0, 205, 0],
    [205, 205, 0],
    [0, 0, 238],
    [205, 0, 205],
    [0, 205, 205],
    [229, 229, 229],
    [127, 127, 127],
    [255, 0, 0],
    [0, 255, 0],
    [255, 255, 0],
    [92, 92, 255],
    [255, 0, 255],
    [0, 255, 255],
    [255, 255, 255],
];
