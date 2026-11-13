//! Cache terminal paint geometry by display row for damage-aware rendering.

use std::sync::Arc;

use iced::advanced::graphics::cache::{Cached, Group};
use iced::advanced::graphics::text::Text;
use iced::font::{Style as FontStyle, Weight};
use iced::{Color, Font, Pixels, Point, Rectangle, Renderer, Size};
use iced_tiny_skia::Geometry;
use kortty_terminal::{Cell, Color as TerminalColor, CursorShape};

use crate::session::{Frame, TypeAheadFrame};
use crate::terminal_theme::Appearance;

use super::{
    CanvasFrame, Metrics, SEARCH_MATCH, SEARCH_SELECTED, SearchHighlight, Selection, cell_colors,
    cursor_bounds, default_background, inline_visual,
};

pub(super) struct View<'a> {
    pub(super) frame: &'a Arc<Frame>,
    pub(super) metrics: Metrics,
    pub(super) appearance: Appearance,
    pub(super) selection: Option<Selection>,
    pub(super) projected_selection: Option<Selection>,
    pub(super) search: Option<SearchHighlight<'a>>,
    pub(super) font: Option<(Font, u8)>,
    pub(super) cursor: Option<(u16, u16)>,
    pub(super) preview: Option<&'a TypeAheadFrame>,
    pub(super) backgrounds: bool,
    pub(super) focus: Option<&'a inline_visual::Focus>,
}

impl View<'_> {
    pub(super) fn dim_at(&self, row: u16) -> f32 {
        self.focus.map_or(0.0, |focus| focus.dim_at(row))
    }
}

#[derive(Clone, Copy, PartialEq)]
struct Style {
    size: Size,
    metrics: Metrics,
    appearance: Appearance,
    font: Option<(Font, u8)>,
    reverse_video: bool,
    backgrounds: bool,
}

#[derive(Default)]
pub(super) struct Rows {
    rows: Vec<Row>,
    frame: Option<Arc<Frame>>,
    style: Option<Style>,
}

struct Row {
    geometry: <Geometry as Cached>::Cache,
    selection: Option<(u16, u16)>,
    highlights: Vec<Highlight>,
    cursor: Option<(u16, CursorShape)>,
    preview: bool,
    dim: f32,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct Highlight {
    start: u16,
    end: u16,
    current: bool,
}

impl Rows {
    pub(super) fn draw(
        &mut self,
        renderer: &Renderer,
        size: Size,
        view: View<'_>,
    ) -> Vec<Geometry> {
        let screen = &view.frame.screen;
        let style = Style {
            size,
            metrics: view.metrics,
            appearance: view.appearance,
            font: view.font,
            reverse_video: screen.reverse_video,
            backgrounds: view.backgrounds,
        };
        let same_style = self.style == Some(style);
        let previous = self.frame.as_ref();
        let same_frame = previous.is_some_and(|frame| Arc::ptr_eq(frame, view.frame));
        let mut search = view
            .search
            .into_iter()
            .flat_map(|search| {
                search.matches.iter().flat_map(move |hit| {
                    let current = search.selected == Some(hit.start);
                    hit.spans.iter().map(move |span| (span, current))
                })
            })
            .peekable();
        let first_row = view.search.map_or(0, |search| search.first_row);
        let canvas = Rectangle::with_size(size);
        let mut visible_rows = 0;
        let mut highlights = Vec::new();
        for row in 0..screen.rows {
            let raw = view.frame.display_raw_row(row);
            let cells = raw.and_then(|raw| screen.row(raw));
            let line = view
                .frame
                .inline
                .as_deref()
                .and_then(|inline| inline.line(row));
            let top = view.metrics.point(row, 0).y;
            if top >= size.height {
                break;
            }
            let clip = row_clip(canvas, top, view.metrics.height);
            highlights.clear();
            if view.backgrounds
                && let Some(raw) = raw
            {
                let absolute = first_row.saturating_add(usize::from(raw));
                while let Some((span, current)) = search.next_if(|(span, _)| span.row <= absolute) {
                    if span.row == absolute {
                        highlights.push(Highlight {
                            start: span.start_column,
                            end: span.end_column,
                            current,
                        });
                    }
                }
            }
            let selection = if let Some(projected) = view.projected_selection {
                row_selection(projected, row)
            } else {
                view.selection
                    .and_then(|selection| row_selection(selection, raw?))
            };
            let cursor = view
                .cursor
                .filter(|(cursor_row, _)| row == *cursor_row)
                .map(|(_, column)| (column, screen.cursor_shape));
            let preview = view.preview.filter(|preview| Some(preview.row) == raw);
            let dim = view.dim_at(row);
            let index = usize::from(row);
            visible_rows = index + 1;
            if same_style
                && let Some(old) = self.rows.get(index)
                && old.selection == selection
                && old.highlights == highlights
                && old.cursor == cursor
                && old.preview == preview.is_some()
                && old.dim == dim
                && (same_frame
                    || previous.is_some_and(|frame| {
                        frame
                            .display_raw_row(row)
                            .and_then(|raw| frame.screen.row(raw))
                            == cells
                            && frame.inline.as_deref().and_then(|inline| inline.line(row)) == line
                    }))
                && (same_frame
                    || preview.is_none_or(|preview| {
                        previous
                            .and_then(|frame| frame.typeahead.as_deref())
                            .is_some_and(|old| old.cells == preview.cells)
                    }))
            {
                continue;
            }
            let mut frame = CanvasFrame::with_bounds(renderer, clip);
            if let Some(cells) = cells {
                if view.backgrounds {
                    paint_backgrounds(&mut frame, &view, row, cells, selection, &highlights);
                }
                if let Some(font) = view.font {
                    let cells = preview.map_or(cells, |preview| &preview.cells);
                    paint_text(&mut frame, &view, row, cells, selection, cursor, font);
                }
            } else if let (Some(line), Some(font)) = (line, view.font) {
                if let Some((start, end)) = selection
                    && start < line.columns() as u16
                {
                    let last = end.min(line.columns() as u16 - 1);
                    let (_, background) = cell_colors(
                        kortty_terminal::Style::default(),
                        screen.reverse_video,
                        true,
                        &view.appearance,
                    );
                    frame.fill_rectangle(
                        view.metrics.point(row, start),
                        Size::new(
                            view.metrics.width * f32::from(last - start + 1),
                            view.metrics.height,
                        ),
                        Color {
                            a: 0.35,
                            ..background
                        },
                    );
                }
                inline_visual::paint_line(&mut frame, &view, row, line, font);
            }
            let mut geometry = frame.into_geometry();
            if let Geometry::Live { text, .. } = &mut geometry {
                for primitive in text {
                    if let Text::Cached { clip_bounds, .. } = primitive {
                        // Iced diffs individual text bounds without consulting the canvas clip.
                        *clip_bounds = clip;
                    }
                }
            }
            let entry = Row {
                geometry: geometry.cache(Group::unique(), None),
                selection,
                highlights: highlights.clone(),
                cursor,
                preview: preview.is_some(),
                dim,
            };
            if let Some(old) = self.rows.get_mut(index) {
                *old = entry;
            } else {
                self.rows.push(entry);
            }
        }
        self.rows.truncate(visible_rows);
        self.frame = Some(Arc::clone(view.frame));
        self.style = Some(style);
        self.rows
            .iter()
            .map(|row| Geometry::load(&row.geometry))
            .collect()
    }
}

fn row_clip(canvas: Rectangle, top: f32, height: f32) -> Rectangle {
    // Allow one line of vertical glyph overhang on each side of the cell.
    let start = (top - height).max(0.0);
    let end = (top + 2.0 * height).min(canvas.height);
    Rectangle::new(Point::new(0.0, start), Size::new(canvas.width, end - start))
}

fn row_selection(selection: Selection, row: u16) -> Option<(u16, u16)> {
    let (first, last) = selection.bounds();
    (first.row..=last.row).contains(&row).then(|| {
        let start = if row == first.row { first.column } else { 0 };
        let end = if row == last.row {
            last.column
        } else {
            u16::MAX
        };
        (start, end)
    })
}

fn selected(selection: Option<(u16, u16)>, column: u16) -> bool {
    selection.is_some_and(|(start, end)| (start..=end).contains(&column))
}

fn paint_backgrounds(
    frame: &mut CanvasFrame,
    view: &View<'_>,
    row: u16,
    cells: &[Cell],
    selection: Option<(u16, u16)>,
    highlights: &[Highlight],
) {
    let screen = &view.frame.screen;
    let default = default_background(screen, &view.appearance);
    let mut matches = highlights.iter().peekable();
    for (column, cell) in cells.iter().enumerate() {
        let column = column as u16;
        while matches
            .next_if(|highlight| highlight.end <= column)
            .is_some()
        {}
        let matched = matches.peek().filter(|highlight| highlight.start <= column);
        let explicit = if cell.style.inverse ^ screen.reverse_video {
            cell.style.foreground != TerminalColor::Default
        } else {
            cell.style.background != TerminalColor::Default
        };
        let selected = selected(selection, column);
        let background = if selected {
            cell_colors(cell.style, screen.reverse_video, true, &view.appearance).1
        } else if let Some(highlight) = matched {
            if highlight.current {
                SEARCH_SELECTED
            } else {
                SEARCH_MATCH
            }
        } else {
            cell_colors(cell.style, screen.reverse_video, false, &view.appearance).1
        };
        if background != default || explicit || selected {
            frame.fill_rectangle(
                view.metrics.point(row, column),
                Size::new(view.metrics.width, view.metrics.height),
                background,
            );
        }
    }
}

fn paint_text(
    frame: &mut CanvasFrame,
    view: &View<'_>,
    row: u16,
    cells: &[Cell],
    selection: Option<(u16, u16)>,
    cursor: Option<(u16, CursorShape)>,
    font: (Font, u8),
) {
    let (font, font_size) = font;
    let metrics = view.metrics;
    let cursor_area =
        cursor.map(|(column, shape)| cursor_bounds(shape, metrics.point(row, column), metrics));
    let block = cursor.filter(|(_, shape)| *shape == CursorShape::Block);
    // The software renderer batches text after shapes in each canvas layer.
    let opacity = 1.0 - view.dim_at(row);
    if block.is_some()
        && let Some((position, size)) = cursor_area
    {
        frame.fill_rectangle(position, size, view.appearance.cursor.color());
    }
    for (column, cell) in cells.iter().enumerate() {
        if cell.width == 0 || cell.style.hidden || cell.text.is_empty() {
            continue;
        }
        let column = column as u16;
        let position = metrics.point(row, column);
        let (mut foreground, background) = cell_colors(
            cell.style,
            view.frame.screen.reverse_video,
            selected(selection, column),
            &view.appearance,
        );
        if block.is_some_and(|(cursor, _)| column == cursor) {
            foreground = background;
        }
        if cell.style.dim {
            foreground.a = 0.6;
        }
        foreground.a *= opacity;
        if cell.text != " " {
            frame.fill_text(iced::widget::canvas::Text {
                content: cell.text.clone(),
                position,
                size: Pixels(f32::from(font_size)),
                color: foreground,
                font: Font {
                    weight: if cell.style.bold && view.appearance.bold_text_mode.uses_bold_font() {
                        Weight::Bold
                    } else {
                        Weight::Normal
                    },
                    style: if cell.style.italic {
                        FontStyle::Italic
                    } else {
                        FontStyle::Normal
                    },
                    ..font
                },
                ..iced::widget::canvas::Text::default()
            });
        }
        let span = Size::new(metrics.width * f32::from(cell.width), 1.0);
        if cell.style.underline {
            frame.fill_rectangle(
                Point::new(position.x, position.y + metrics.height - 3.0),
                span,
                foreground,
            );
        }
        if cell.style.strikethrough {
            frame.fill_rectangle(
                Point::new(position.x, position.y + metrics.height / 2.0),
                span,
                foreground,
            );
        }
    }
    if block.is_none()
        && let Some((position, size)) = cursor_area
    {
        frame.fill_rectangle(position, size, view.appearance.cursor.color());
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use iced::advanced::graphics::{Viewport, damage, geometry::Renderer as _};
    use iced::advanced::renderer::Renderer as _;
    use iced::widget::canvas::Program as _;
    use iced::{Color, Theme, mouse, window};
    use iced_tiny_skia::Layer;
    use kortty_terminal::Terminal;

    use crate::session::inline_history;
    use crate::terminal_agent::design::inline::{Line, Role, Segment};
    use crate::terminal_view::{State, TerminalView, selection_test_frame};

    use super::*;

    #[test]
    fn terminal_row_damage_matches_full_render_after_text_and_cursor_move() {
        let viewport = Viewport::with_physical_size(Size::new(420, 360), 2.0);
        let bounds = Rectangle::with_size(viewport.logical_size());
        let mut renderer = Renderer::new(Font::MONOSPACE, Pixels(14.0));
        let mut terminal = Terminal::new(20, 9);
        let mut mask = tiny_skia::Mask::new(420, 360).unwrap();
        let mut partial = tiny_skia::Pixmap::new(420, 360).unwrap();
        let mut full = tiny_skia::Pixmap::new(420, 360).unwrap();
        let state = State::default();
        let owner = window::Id::unique();
        terminal.advance(
            "first\r\n\x1b[3;4;33;44mBär 界\x1b[0m\r\nthird\x1b[8;1Hstays\x1b[2;3H".as_bytes(),
        );

        let render = |renderer: &mut Renderer, frame: &Arc<Frame>| {
            renderer.reset(bounds);
            let view = TerminalView {
                session: crate::session::SessionId(1),
                frame,
                font_size: 14,
                font: Font::MONOSPACE,
                appearance: Appearance::default(),
                background_image: None,
                scale_factor: 2.0,
                background_transparency: 40,
                focused: true,
                reduced_motion: false,
                typeahead_enabled: true,
                owner,
                plain_text_links: false,
                selection: None,
                projected: None,
                search: None,
                agent_cursor: None,
                inline_agent: None,
            };
            for geometry in view.draw(
                &state,
                renderer,
                &Theme::Dark,
                bounds,
                mouse::Cursor::Unavailable,
            ) {
                renderer.draw_geometry(geometry);
            }
            renderer.layers().to_vec()
        };

        let first = selection_test_frame(terminal.snapshot());
        let old_layers = render(&mut renderer, &first);
        renderer.draw(
            &mut partial.as_mut(),
            &mut mask,
            &viewport,
            &[bounds],
            Color::TRANSPARENT,
        );
        let diff = |old: &[Layer], new: &[Layer]| {
            damage::group(
                damage::diff(old, new, |layer| vec![layer.bounds], Layer::damage),
                bounds,
            )
        };
        assert!(diff(&old_layers, &render(&mut renderer, &first)).is_empty());

        terminal.advance(b"X\x1b[3;3H");
        let updated_layers = render(&mut renderer, &selection_test_frame(terminal.snapshot()));
        let damage = diff(&old_layers, &updated_layers);
        assert!(!damage.is_empty());
        assert!(
            damage
                .iter()
                .all(|area| area.y + area.height < bounds.height / 2.0)
        );
        renderer.draw(
            &mut partial.as_mut(),
            &mut mask,
            &viewport,
            &damage,
            Color::TRANSPARENT,
        );
        renderer.draw(
            &mut full.as_mut(),
            &mut mask,
            &viewport,
            &[bounds],
            Color::TRANSPARENT,
        );
        let first_difference = partial
            .data()
            .iter()
            .zip(full.data())
            .position(|(a, b)| a != b);
        assert_eq!(first_difference, None);
    }

    #[test]
    fn agent_projection_places_role_text_on_display_rows_and_keeps_the_prompt_bright() {
        let mut terminal = Terminal::new(24, 7);
        terminal.advance(b"A\x1b[3;1HB\x1b[4;1H$ ");
        let mut frame = selection_test_frame(terminal.snapshot());
        inline_history::fixture(
            Arc::get_mut(&mut frame).unwrap(),
            2,
            vec![Line {
                segments: vec![
                    Segment {
                        text: "  ".into(),
                        role: Role::Text,
                    },
                    Segment {
                        text: "Agent".into(),
                        role: Role::Title,
                    },
                ],
            }],
            true,
        );
        let view = TerminalView {
            session: crate::session::SessionId(1),
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
            owner: window::Id::unique(),
            plain_text_links: true,
            selection: None,
            projected: None,
            search: None,
            agent_cursor: None,
            inline_agent: None,
        };
        let state = State::default();
        let renderer = Renderer::new(Font::MONOSPACE, Pixels(14.0));
        let bounds = Rectangle::with_size(Size::new(360.0, 160.0));
        let geometry = view.draw(
            &state,
            &renderer,
            &Theme::Dark,
            bounds,
            mouse::Cursor::Unavailable,
        );
        let cell = Metrics::with_font(view.font_size, view.font);
        let text_at = |row: u16, expected: &str, column: u16, opacity: f32| {
            let Geometry::Cache(cache) = &geometry[usize::from(row) + 1] else {
                panic!("expected cached row geometry");
            };
            assert!(cache.text.iter().any(|text| matches!(
                text,
                Text::Cached { content, bounds, color, .. }
                    if content == expected
                        && bounds.position() == cell.point(row, column)
                        && (color.a - opacity).abs() < 0.001
            )));
        };
        text_at(0, "A", 0, 0.42);
        text_at(2, "Agent", 2, 1.0);
        text_at(3, "B", 0, 1.0);
        text_at(4, "$", 0, 1.0);
        assert_eq!(view.display_cursor(), Some((4, 2)));
        assert!(
            matches!(geometry.last(), Some(Geometry::Live { primitives, .. }) if primitives.len() >= 3)
        );
        assert!(inline_visual::next_redraw(&view, Instant::now()).is_none());
    }
}
