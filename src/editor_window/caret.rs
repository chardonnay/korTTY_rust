use std::fmt;

use iced::advanced::text::{self, Highlighter, Renderer as _};
use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Layout, Renderer as _, Shell, Widget, layout, renderer};
use iced::widget::{TextEditor, text_editor};
use iced::{
    Element, Event, Font, Length, Padding, Pixels, Point, Rectangle, Renderer, Size, Theme, mouse,
};
use iced_tiny_skia::{TextEditorCaret, TextEditorCaretShape};
use serde::{Deserialize, Serialize};

use crate::terminal_theme::{Rgb, cell_width_ratio};

use super::Appearance;

const PADDING: f32 = 5.0;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Shape {
    #[default]
    Block,
    Line,
    Underscore,
}

impl Shape {
    pub(crate) const ALL: [Self; 3] = [Self::Block, Self::Line, Self::Underscore];

    fn renderer_shape(self) -> TextEditorCaretShape {
        match self {
            Self::Block => TextEditorCaretShape::Block,
            Self::Line => TextEditorCaretShape::Line,
            Self::Underscore => TextEditorCaretShape::Underline,
        }
    }
}

impl fmt::Display for Shape {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Block => "Block",
            Self::Line => "Line",
            Self::Underscore => "Underscore",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct Preferences {
    pub(crate) shape: Shape,
    pub(crate) color: Rgb,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            shape: Shape::Block,
            color: Rgb::from_hex(0xff0000),
        }
    }
}

impl Preferences {
    pub(crate) fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

#[cfg(test)]
fn view<'a, H: Highlighter, Message: 'a>(
    editor: TextEditor<'a, H, Message>,
    content_theme: Theme,
    appearance: Appearance,
    font_size: u8,
    preferences: Preferences,
) -> Element<'a, Message> {
    view_decorated(
        editor,
        content_theme,
        appearance,
        font_size,
        preferences,
        Decorations::default(),
    )
}

#[cfg(test)]
pub(crate) fn view_with_layout<'a, H: Highlighter, Message: 'a>(
    editor: TextEditor<'a, H, Message>,
    content_theme: Theme,
    appearance: Appearance,
    font_size: u8,
    preferences: Preferences,
    numbered_content: Option<&'a text_editor::Content>,
    word_wrap: Option<bool>,
) -> Element<'a, Message> {
    view_decorated(
        editor,
        content_theme,
        appearance,
        font_size,
        preferences,
        Decorations {
            numbered_content,
            word_wrap,
            ..Decorations::default()
        },
    )
}

#[derive(Default)]
pub(crate) struct Decorations<'a> {
    pub(crate) numbered_content: Option<&'a text_editor::Content>,
    pub(crate) word_wrap: Option<bool>,
    pub(crate) diagnostics: Option<(
        &'a text_editor::Content,
        &'a [crate::snippets::json_schema::Mark],
    )>,
}

pub(crate) fn view_decorated<'a, H: Highlighter, Message: 'a>(
    editor: TextEditor<'a, H, Message>,
    content_theme: Theme,
    appearance: Appearance,
    font_size: u8,
    preferences: Preferences,
    decorations: Decorations<'a>,
) -> Element<'a, Message> {
    let Decorations {
        numbered_content,
        word_wrap,
        diagnostics,
    } = decorations;
    let gutter = numbered_content.map(|content| Gutter {
        content,
        font: appearance.font,
        size: Pixels(f32::from(font_size)),
        width: content.line_count().max(1).ilog10() as f32 * f32::from(font_size)
            + f32::from(font_size)
            + PADDING,
    });
    let padding = Padding {
        left: PADDING + gutter.as_ref().map_or(0.0, |gutter| gutter.width),
        ..Padding::new(PADDING)
    };
    let editor = match word_wrap {
        Some(true) => editor.wrapping(text::Wrapping::WordOrGlyph),
        Some(false) => editor.horizontal_scroll(horizontal_metrics),
        None => editor,
    };
    let editor = editor
        .font(appearance.font)
        .size(f32::from(font_size))
        .padding(padding)
        .style(move |_, status| iced::widget::text_editor::Style {
            value: TextEditorCaret::VALUE_COLOR,
            ..appearance.style(&content_theme, status)
        });
    let padding = editor.content_padding();
    crate::workspace::ui_font_scale::unscaled(Element::new(CaretEditor {
        editor: editor.into(),
        padding,
        gutter,
        diagnostics: diagnostics.map(|(content, marks)| {
            let positions = marks
                .iter()
                .map(|mark| {
                    (
                        super::position_after_edit(content, mark.bytes.start),
                        super::position_after_edit(content, mark.bytes.end),
                    )
                })
                .collect();
            (content, positions)
        }),
        editor_geometry: |tree| {
            let state = tree.state.downcast_ref::<text_editor::State<H>>();
            (state.inline_gap(), state.horizontal_offset())
        },
        caret: TextEditorCaret {
            foreground: appearance.foreground.color(),
            color: preferences.color.color(),
            shape: preferences.shape.renderer_shape(),
            clip_bounds: None,
            fallback_cell_width: cell_width_ratio(appearance.font) * f32::from(font_size),
            use_widget_x: word_wrap == Some(false),
        },
    }))
}

fn horizontal_metrics(
    editor: &<Renderer as text::Renderer>::Editor,
) -> text_editor::HorizontalMetrics {
    use text::editor::Editor as _;

    let cursor = editor.cursor().position;
    let buffer = editor.buffer();
    let mut metrics = text_editor::HorizontalMetrics::default();
    for run in buffer.layout_runs() {
        let left = if run.rtl {
            let right = buffer.size().0.unwrap_or(run.line_w);
            metrics.left = metrics.left.min(right - run.line_w);
            right - run.line_w
        } else {
            metrics.right = metrics.right.max(run.line_w);
            0.0
        };
        if run.line_i == cursor.line {
            metrics.caret_x = Some(
                run.glyphs
                    .iter()
                    .find(|glyph| (glyph.start..glyph.end).contains(&cursor.column))
                    .map_or(if run.rtl { left } else { run.line_w }, |glyph| {
                        glyph.x + if glyph.level.is_rtl() { glyph.w } else { 0.0 }
                    }),
            );
        }
    }
    metrics
}

struct CaretEditor<'a, Message> {
    editor: Element<'a, Message>,
    caret: TextEditorCaret,
    padding: Padding,
    gutter: Option<Gutter<'a>>,
    diagnostics: Option<(&'a text_editor::Content, Vec<DiagnosticPosition>)>,
    editor_geometry: fn(&Tree) -> (Option<text_editor::InlineGap>, f32),
}

type DiagnosticPosition = (text_editor::Position, text_editor::Position);

pub(crate) fn reveal_diagnostic(content: &mut text_editor::Content, bytes: std::ops::Range<usize>) {
    let start = super::position_after_edit(content, bytes.start);
    let end = super::position_after_edit(content, bytes.end);
    content.move_to(text_editor::Cursor {
        position: end,
        selection: (start != end).then_some(start),
    });
}

fn draw_diagnostics(
    renderer: &mut Renderer,
    content: &text_editor::Content,
    positions: &[DiagnosticPosition],
    bounds: Rectangle,
    viewport: Rectangle,
    (gap, horizontal): (Option<text_editor::InlineGap>, f32),
    color: iced::Color,
) {
    let editor = content.editor();
    let buffer = editor.buffer();
    let height = buffer.metrics().line_height;
    let mut paint = |x: f32, y: f32, width: f32| {
        let line = Rectangle::new(
            Point::new(bounds.x + x - horizontal, bounds.y + y + height - 2.0),
            Size::new(width.max(2.0), 2.0),
        );
        if let Some(bounds) = line.intersection(&viewport) {
            renderer.fill_quad(
                renderer::Quad {
                    bounds,
                    ..renderer::Quad::default()
                },
                color,
            );
        }
    };
    for run in buffer.layout_runs() {
        let y = gap.map_or(run.line_top, |gap| gap.row_top(run.line_top));
        for &(start, end) in positions {
            if run.line_i != start.line {
                continue;
            }
            if start == end {
                if let Some(glyph) = run
                    .glyphs
                    .iter()
                    .find(|glyph| (glyph.start..glyph.end).contains(&start.column))
                {
                    paint(glyph.x, y, glyph.w);
                } else if run.glyphs.iter().map(|glyph| glyph.end).max().unwrap_or(0)
                    == start.column
                {
                    let right = run
                        .glyphs
                        .iter()
                        .map(|glyph| glyph.x + glyph.w)
                        .fold(0.0, f32::max);
                    paint(right, y, 6.0);
                }
            } else {
                let end = if start.line == end.line {
                    end.column
                } else {
                    usize::MAX
                };
                for glyph in run
                    .glyphs
                    .iter()
                    .filter(|glyph| glyph.end > start.column && glyph.start < end)
                {
                    paint(glyph.x, y, glyph.w);
                }
            }
        }
    }
}

struct Gutter<'a> {
    content: &'a text_editor::Content,
    font: Font,
    size: Pixels,
    width: f32,
}

impl Gutter<'_> {
    fn draw(
        &self,
        renderer: &mut Renderer,
        bounds: Rectangle,
        viewport: &Rectangle,
        color: iced::Color,
        inline_gap: Option<text_editor::InlineGap>,
    ) {
        let mut gutter = bounds.shrink(PADDING);
        gutter.width = gutter.width.min(self.width - PADDING);
        let editor = self.content.editor();
        let buffer = editor.buffer();
        if let Some(height) = buffer.size().1 {
            gutter.height = gutter.height.min(height);
        }
        let Some(clip) = gutter.intersection(viewport) else {
            return;
        };
        let row_height = buffer.metrics().line_height;
        let line_height = text::LineHeight::Absolute(Pixels(row_height));
        let paint = |renderer: &mut Renderer, line: usize, top: f32| {
            renderer.fill_text(
                text::Text {
                    content: (line + 1).to_string(),
                    bounds: Size::new(gutter.width, row_height),
                    size: self.size,
                    line_height,
                    font: self.font,
                    align_x: text::Alignment::Right,
                    align_y: iced::alignment::Vertical::Top,
                    shaping: text::Shaping::Advanced,
                    wrapping: text::Wrapping::None,
                },
                Point::new(gutter.x + gutter.width, gutter.y + top),
                color,
                clip,
            );
        };
        let scroll = buffer.scroll();
        let mut previous = None;
        for run in buffer.layout_runs() {
            if previous == Some(run.line_i) {
                continue;
            }
            previous = Some(run.line_i);
            // The iterator can begin on a wrapped continuation after scrolling.
            if run.line_i != scroll.line || run.line_top <= -scroll.vertical {
                let top = inline_gap.map_or(run.line_top, |gap| gap.row_top(run.line_top));
                paint(renderer, run.line_i, top);
            }
        }
        if previous.is_none() && buffer.lines.len() <= 1 {
            paint(renderer, 0, 0.0);
        }
    }
}

impl<Message> Widget<Message, Theme, Renderer> for CaretEditor<'_, Message> {
    fn tag(&self) -> tree::Tag {
        self.editor.as_widget().tag()
    }

    fn state(&self) -> tree::State {
        self.editor.as_widget().state()
    }

    fn diff(&self, tree: &mut Tree) {
        self.editor.as_widget().diff(tree);
    }

    fn size(&self) -> Size<Length> {
        self.editor.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.editor.as_widget_mut().layout(tree, renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.editor
            .as_widget_mut()
            .operate(tree, layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.editor.as_widget_mut().update(
            tree, event, layout, cursor, renderer, clipboard, shell, viewport,
        );
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let caret = TextEditorCaret {
            clip_bounds: layout.bounds().shrink(self.padding).intersection(viewport),
            ..self.caret
        };
        renderer.with_text_editor_caret(caret, |renderer| {
            self.editor
                .as_widget()
                .draw(tree, renderer, theme, style, layout, cursor, viewport);
        });
        let geometry = (self.editor_geometry)(tree);
        if let Some((content, positions)) = &self.diagnostics
            && let Some(clip) = caret.clip_bounds
        {
            let bounds = layout.bounds().shrink(self.padding);
            renderer.with_layer(clip, |renderer| {
                draw_diagnostics(
                    renderer,
                    content,
                    positions,
                    bounds,
                    clip,
                    geometry,
                    theme.extended_palette().danger.base.color,
                )
            });
        }
        if let Some(gutter) = &self.gutter {
            gutter.draw(
                renderer,
                layout.bounds(),
                viewport,
                self.caret.foreground,
                geometry.0,
            );
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.editor
            .as_widget()
            .mouse_interaction(tree, layout, cursor, viewport, renderer)
    }
}

#[cfg(test)]
mod tests {
    use iced::advanced::graphics::Viewport;
    use iced::advanced::widget::operation::focusable;
    use iced::widget::{self, text_editor};
    use iced::{Background, Color, Font, Pixels};

    use super::*;

    #[test]
    fn diagnostic_marks_draw_on_their_source_line_and_reveal_the_exact_token() {
        let source = "{\r\n  \"é\": false\r\n}";
        let mut content = text_editor::Content::with_text(source);
        let start = source.find("false").unwrap();
        let bytes = start..start + 5;
        let marks = [crate::snippets::json_schema::Mark {
            label: "/é".into(),
            bytes: bytes.clone(),
        }];
        let appearance = Appearance {
            font: Font::MONOSPACE,
            size: 16,
            foreground: Rgb::from_hex(0xffffff),
            background: Rgb::from_hex(0x000000),
        };
        let mut renderer = Renderer::new(Font::MONOSPACE, Pixels(16.0));
        let bounds = Rectangle::with_size(Size::new(200.0, 100.0));
        let mut editor = view_decorated(
            text_editor(&content).height(Length::Fill).on_action(|_| ()),
            Theme::Dark,
            appearance,
            16,
            Preferences::default(),
            Decorations {
                diagnostics: Some((&content, &marks)),
                ..Decorations::default()
            },
        );
        let mut tree = Tree::new(editor.as_widget());
        let node = editor.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, bounds.size()),
        );
        renderer.reset(bounds);
        editor.as_widget().draw(
            &tree,
            &mut renderer,
            &Theme::Dark,
            &renderer::Style::default(),
            Layout::new(&node),
            mouse::Cursor::Unavailable,
            &bounds,
        );
        let danger = Background::Color(Theme::Dark.extended_palette().danger.base.color);
        let row = content.editor().buffer().metrics().line_height;
        let underlines: Vec<_> = renderer
            .layers()
            .iter()
            .flat_map(|layer| &layer.quads)
            .filter_map(|(quad, color)| (*color == danger).then_some(quad.bounds))
            .collect();
        assert!(!underlines.is_empty());
        assert!(
            underlines
                .iter()
                .all(|area| (area.y - (PADDING + row * 2.0 - 2.0)).abs() < 0.1 && area.x > PADDING)
        );
        drop(editor);
        reveal_diagnostic(&mut content, bytes);
        assert_eq!(content.selection().as_deref(), Some("false"));
        assert_eq!(content.text(), source);
    }

    #[test]
    fn multiline_inline_suggestion_moves_source_rows_and_maps_clicks_without_editing() {
        let viewport = Viewport::with_physical_size(Size::new(220, 110), 1.0);
        let bounds = Rectangle::with_size(viewport.logical_size());
        let mut renderer = Renderer::new(Font::MONOSPACE, Pixels(16.0));
        let mut content = text_editor::Content::with_text("ra\nEND");
        content.move_to(text_editor::Cursor {
            position: text_editor::Position { line: 0, column: 2 },
            selection: None,
        });
        let appearance = Appearance {
            font: Font::MONOSPACE,
            size: 16,
            foreground: Rgb::from_hex(0xffffff),
            background: Rgb::from_hex(0x000000),
        };
        let mut render = |suggestion, focused| {
            let id = widget::Id::unique();
            let mut editor = view_with_layout(
                text_editor(&content)
                    .id(id.clone())
                    .height(Length::Fill)
                    .inline_suggestion(suggestion)
                    .on_action(|action| action),
                Theme::Dark,
                appearance,
                appearance.size,
                Preferences::default(),
                Some(&content),
                Some(false),
            );
            let mut tree = Tree::new(editor.as_widget());
            let limits = layout::Limits::new(Size::ZERO, bounds.size());
            let node = editor.as_widget_mut().layout(&mut tree, &renderer, &limits);
            let layout = Layout::new(&node);
            if focused {
                editor.as_widget_mut().operate(
                    &mut tree,
                    layout,
                    &renderer,
                    &mut focusable::focus::<()>(id),
                );
            }
            renderer.reset(bounds);
            editor.as_widget().draw(
                &tree,
                &mut renderer,
                &Theme::Dark,
                &renderer::Style::default(),
                layout,
                mouse::Cursor::Unavailable,
                &bounds,
            );
            let mut pixmap = tiny_skia::Pixmap::new(220, 110).unwrap();
            let mut mask = tiny_skia::Mask::new(220, 110).unwrap();
            renderer.draw(
                &mut pixmap.as_mut(),
                &mut mask,
                &viewport,
                &[bounds],
                Color::TRANSPARENT,
            );
            let row_height = content.editor().buffer().metrics().line_height;
            let mut messages = Vec::new();
            let mut clipboard = iced::advanced::clipboard::Null;
            let mouse = mouse::Cursor::Available(Point::new(70.0, PADDING + 3.5 * row_height));
            editor.as_widget_mut().update(
                &mut tree,
                &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                layout,
                mouse,
                &renderer,
                &mut clipboard,
                &mut Shell::new(&mut messages),
                &bounds,
            );
            (pixmap, messages, row_height)
        };
        let suggestion = "nge(20):\n    SECOND\n    THIRD";
        let (focused, click, row_height) = render(Some(suggestion), true);
        let (baseline, _, _) = render(None, true);
        let bright = |image: &tiny_skia::Pixmap, row: u32, columns: std::ops::Range<u32>| {
            let top = (PADDING + row as f32 * row_height).ceil() as u32;
            let bottom = (PADDING + (row + 1) as f32 * row_height).floor() as u32;
            (top..bottom).any(|y| {
                columns
                    .clone()
                    .any(|x| image.pixel(x, y).unwrap().red() > 230)
            })
        };
        assert!((8..28).any(|y| (50..180).any(|x| focused.pixel(x, y) != baseline.pixel(x, y))));
        for columns in [4..22, 26..62] {
            assert!(bright(&baseline, 1, columns.clone()));
            assert!(!bright(&focused, 1, columns.clone()));
            assert!(bright(&focused, 3, columns));
        }
        let top = (PADDING + 2.0 * row_height).ceil() as u32;
        let bottom = (PADDING + 3.0 * row_height).floor() as u32;
        assert!(
            (top..bottom).any(|y| (70..170).any(|x| focused.pixel(x, y) != baseline.pixel(x, y)))
        );
        let [text_editor::Action::Click(point)] = click.as_slice() else {
            panic!("expected an editor click");
        };
        assert!((point.y - row_height * 1.5).abs() < 0.1);
        assert_eq!(
            render(Some(suggestion), false).0.data(),
            render(None, false).0.data()
        );
        assert_eq!(content.text(), "ra\nEND");
    }

    #[test]
    fn unwrapped_editor_scrolls_beyond_its_width_and_translates_clicks() {
        fn build(
            content: &text_editor::Content,
            appearance: Appearance,
        ) -> Element<'_, text_editor::Action> {
            view_with_layout(
                text_editor(content)
                    .height(Length::Fill)
                    .on_action(|action| action),
                Theme::Dark,
                appearance,
                appearance.size,
                Preferences::default(),
                None,
                Some(false),
            )
        }

        let viewport = Viewport::with_physical_size(Size::new(120, 60), 1.0);
        let bounds = Rectangle::with_size(viewport.logical_size());
        let mut renderer = Renderer::new(Font::MONOSPACE, Pixels(16.0));
        let mut content = text_editor::Content::with_text(&format!("{}TAIL", " ".repeat(80)));
        let appearance = Appearance {
            font: Font::MONOSPACE,
            size: 16,
            foreground: Rgb::from_hex(0xffffff),
            background: Rgb::from_hex(0x000000),
        };
        let mut editor = build(&content, appearance);
        let mut tree = Tree::new(editor.as_widget());
        let limits = layout::Limits::new(Size::ZERO, bounds.size());
        let node = editor.as_widget_mut().layout(&mut tree, &renderer, &limits);
        let draw = |editor: &Element<'_, text_editor::Action>,
                    tree: &Tree,
                    renderer: &mut Renderer,
                    node: &layout::Node| {
            renderer.reset(bounds);
            editor.as_widget().draw(
                tree,
                renderer,
                &Theme::Dark,
                &renderer::Style::default(),
                Layout::new(node),
                mouse::Cursor::Unavailable,
                &bounds,
            );
            let mut pixmap = tiny_skia::Pixmap::new(120, 60).unwrap();
            let mut mask = tiny_skia::Mask::new(120, 60).unwrap();
            renderer.draw(
                &mut pixmap.as_mut(),
                &mut mask,
                &viewport,
                &[bounds],
                Color::TRANSPARENT,
            );
            (8..31).any(|y| (6..114).any(|x| pixmap.pixel(x, y).unwrap().red() > 100))
        };
        assert!(!draw(&editor, &tree, &mut renderer, &node));
        let cursor = mouse::Cursor::Available(Point::new(95.0, 13.0));
        let mut messages = Vec::new();
        let track = mouse::Cursor::Available(Point::new(114.0, 50.0));
        for (event, cursor, tail_visible) in [
            (
                mouse::Event::ButtonPressed(mouse::Button::Left),
                track,
                true,
            ),
            (
                mouse::Event::ButtonReleased(mouse::Button::Left),
                track,
                true,
            ),
            (
                mouse::Event::WheelScrolled {
                    delta: mouse::ScrollDelta::Pixels { x: 2000.0, y: 0.0 },
                },
                cursor,
                false,
            ),
            (
                mouse::Event::WheelScrolled {
                    delta: mouse::ScrollDelta::Pixels { x: -2000.0, y: 0.0 },
                },
                cursor,
                true,
            ),
        ] {
            editor.as_widget_mut().update(
                &mut tree,
                &Event::Mouse(event),
                Layout::new(&node),
                cursor,
                &renderer,
                &mut iced::advanced::clipboard::Null,
                &mut Shell::new(&mut messages),
                &bounds,
            );
            assert!(messages.is_empty());
            assert_eq!(draw(&editor, &tree, &mut renderer, &node), tail_visible);
        }
        editor.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            Layout::new(&node),
            cursor,
            &renderer,
            &mut iced::advanced::clipboard::Null,
            &mut Shell::new(&mut messages),
            &bounds,
        );
        let [text_editor::Action::Click(click)] = messages.as_slice() else {
            panic!("expected repositioned click, got {messages:?}");
        };
        assert!(click.x > bounds.width);
        drop(editor);
        content.perform(text_editor::Action::Click(*click));
        assert!(content.cursor().position.column >= 80);
        content.perform(text_editor::Action::Move(text_editor::Motion::End));
        let mut editor = build(&content, appearance);
        let mut tree = Tree::new(editor.as_widget());
        let node = editor.as_widget_mut().layout(&mut tree, &renderer, &limits);
        assert!(draw(&editor, &tree, &mut renderer, &node));

        let long_token = text_editor::Content::with_text(&"W".repeat(84));
        let mut wrapped: Element<'_, ()> = view_with_layout(
            text_editor(&long_token)
                .height(Length::Fill)
                .on_action(|_| ()),
            Theme::Dark,
            appearance,
            appearance.size,
            Preferences::default(),
            None,
            Some(true),
        );
        let mut tree = Tree::new(wrapped.as_widget());
        let _ = wrapped
            .as_widget_mut()
            .layout(&mut tree, &renderer, &limits);
        let editor = long_token.editor();
        assert!(
            editor
                .buffer()
                .layout_runs()
                .filter(|run| run.line_i == 0)
                .count()
                > 1
        );

        let right_aligned = text_editor::Content::with_text(&"אבג".repeat(40));
        let mut editor = build(&right_aligned, appearance);
        let mut tree = Tree::new(editor.as_widget());
        let node = editor.as_widget_mut().layout(&mut tree, &renderer, &limits);
        let mut messages = Vec::new();
        for event in [
            mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Pixels { x: 2000.0, y: 0.0 },
            },
            mouse::Event::ButtonPressed(mouse::Button::Left),
        ] {
            editor.as_widget_mut().update(
                &mut tree,
                &Event::Mouse(event),
                Layout::new(&node),
                cursor,
                &renderer,
                &mut iced::advanced::clipboard::Null,
                &mut Shell::new(&mut messages),
                &bounds,
            );
        }
        let [text_editor::Action::Click(click)] = messages.as_slice() else {
            panic!("expected click in right-aligned text, got {messages:?}");
        };
        assert!(click.x < 0.0);
    }

    #[test]
    fn numbered_editor_paints_only_logical_line_starts_even_with_wrapping_and_scroll() {
        let viewport = Viewport::with_physical_size(Size::new(120, 92), 1.0);
        let bounds = Rectangle::with_size(viewport.logical_size());
        let mut renderer = Renderer::new(Font::MONOSPACE, Pixels(16.0));
        let mut content = text_editor::Content::with_text("one two three four five\nnext\nlast");
        let appearance = Appearance {
            font: Font::MONOSPACE,
            size: 16,
            foreground: Rgb::from_hex(0xffffff),
            background: Rgb::from_hex(0x000000),
        };
        let render = |renderer: &mut Renderer, content: &text_editor::Content| {
            let mut editor: Element<'_, ()> = view_with_layout(
                text_editor(content).height(Length::Fill).on_action(|_| ()),
                Theme::Dark,
                appearance,
                appearance.size,
                Preferences::default(),
                Some(content),
                Some(true),
            );
            let mut tree = Tree::new(editor.as_widget());
            let limits = layout::Limits::new(Size::ZERO, bounds.size());
            let node = editor.as_widget_mut().layout(&mut tree, renderer, &limits);
            renderer.reset(bounds);
            editor.as_widget().draw(
                &tree,
                renderer,
                &Theme::Dark,
                &renderer::Style::default(),
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &bounds,
            );
            let mut pixmap = tiny_skia::Pixmap::new(120, 92).unwrap();
            let mut mask = tiny_skia::Mask::new(120, 92).unwrap();
            renderer.draw(
                &mut pixmap.as_mut(),
                &mut mask,
                &viewport,
                &[bounds],
                Color::TRANSPARENT,
            );
            pixmap
        };
        let pixels = render(&mut renderer, &content);
        let has_number = |pixels: &tiny_skia::Pixmap, top: f32, height: f32| {
            ((PADDING + top).max(PADDING) as u32..(PADDING + top + height) as u32)
                .any(|y| (5..21).any(|x| pixels.pixel(x, y).is_some_and(|pixel| pixel.red() > 100)))
        };
        {
            let editor = content.editor();
            let buffer = editor.buffer();
            let runs: Vec<_> = buffer.layout_runs().collect();
            assert!(runs.iter().filter(|run| run.line_i == 0).count() > 1);
            let mut previous = None;
            for run in runs {
                if run.line_top + run.line_height <= bounds.height - 2.0 * PADDING {
                    assert_eq!(
                        has_number(&pixels, run.line_top, run.line_height),
                        previous != Some(run.line_i)
                    );
                }
                previous = Some(run.line_i);
            }
        }
        content.perform(text_editor::Action::Scroll { lines: 1 });
        let pixels = render(&mut renderer, &content);
        let editor = content.editor();
        let buffer = editor.buffer();
        let mut runs = buffer.layout_runs();
        let first = runs.next().unwrap();
        assert_eq!(first.line_i, 0);
        assert!(!has_number(&pixels, first.line_top, first.line_height));
        let second_line = runs.find(|run| run.line_i == 1).unwrap();
        assert!(has_number(
            &pixels,
            second_line.line_top,
            second_line.line_height
        ));
    }

    #[test]
    fn file_editor_caret_keeps_the_block_glyph_visible_and_respects_selection() {
        let viewport = Viewport::with_physical_size(Size::new(140, 80), 1.0);
        let bounds = Rectangle::with_size(viewport.logical_size());
        let mut renderer = Renderer::new(Font::MONOSPACE, Pixels(24.0));
        let content = text_editor::Content::with_text("MMMMMMMMMMMMMMMM");
        let mut selected = text_editor::Content::with_text("MMMMMMMMMMMMMMMM");
        selected.move_to(text_editor::Cursor {
            position: text_editor::Position { line: 0, column: 1 },
            selection: Some(text_editor::Position { line: 0, column: 0 }),
        });
        let appearance = Appearance {
            font: Font::MONOSPACE,
            size: 24,
            foreground: Rgb::from_hex(0xff0000),
            background: Rgb::from_hex(0x102030),
        };
        let draw = |renderer: &mut Renderer,
                    content: &text_editor::Content,
                    shape: Option<Shape>,
                    focused| {
            let id = widget::Id::unique();
            let editor = text_editor(content)
                .id(id.clone())
                .on_action(|_| ())
                .height(Length::Fill)
                .highlight_with::<crate::editor_window::lua::Lua>(
                    crate::editor_window::lua::Settings::new(true, appearance, 0),
                    crate::editor_window::lua::format,
                );
            let mut editor: Element<'_, ()> = if let Some(shape) = shape {
                view(
                    editor,
                    Theme::Dark,
                    appearance,
                    appearance.size,
                    Preferences {
                        shape,
                        ..Preferences::default()
                    },
                )
            } else {
                editor
                    .font(appearance.font)
                    .size(f32::from(appearance.size))
                    .style(move |theme, status| appearance.style(theme, status))
                    .into()
            };
            let mut tree = Tree::new(editor.as_widget());
            let limits = layout::Limits::new(Size::ZERO, bounds.size());
            let node = editor.as_widget_mut().layout(&mut tree, renderer, &limits);
            let layout = Layout::new(&node);
            if focused {
                editor.as_widget_mut().operate(
                    &mut tree,
                    layout,
                    renderer,
                    &mut focusable::focus::<()>(id),
                );
            }
            assert_eq!(
                tree.state
                    .downcast_ref::<text_editor::State<crate::editor_window::lua::Lua>>()
                    .is_focused(),
                focused,
            );
            renderer.reset(bounds);
            editor.as_widget().draw(
                &tree,
                renderer,
                &Theme::Dark,
                &renderer::Style::default(),
                layout,
                mouse::Cursor::Unavailable,
                &bounds,
            );
            let red = Background::Color(Preferences::default().color.color());
            let quads = renderer.layers().iter().flat_map(|layer| &layer.quads);
            let caret = quads
                .clone()
                .filter_map(|(quad, color)| (*color == red).then_some(quad.bounds))
                .collect::<Vec<_>>();
            if focused && content.cursor().selection.is_none() {
                assert_eq!(
                    caret.len(),
                    1,
                    "{shape:?} caret at {:?}, editor bounds {:?}, quads {:?}",
                    content.cursor(),
                    layout.bounds(),
                    quads.collect::<Vec<_>>()
                );
            }
            caret
        };
        let stock_caret = draw(&mut renderer, &content, None, true)[0];
        let mut painted = Vec::new();
        for shape in Shape::ALL {
            assert!(draw(&mut renderer, &content, Some(shape), false).is_empty());
            let caret = draw(&mut renderer, &content, Some(shape), true);
            assert_eq!(caret.len(), 1);
            let caret = caret[0];
            painted.push(caret);

            if shape == Shape::Block {
                let mut pixels = tiny_skia::Pixmap::new(140, 80).unwrap();
                let mut mask = tiny_skia::Mask::new(140, 80).unwrap();
                renderer.draw(
                    &mut pixels.as_mut(),
                    &mut mask,
                    &viewport,
                    &[bounds],
                    Color::TRANSPARENT,
                );
                let ink =
                    (caret.y.ceil() as u32..(caret.y + caret.height).floor() as u32).any(|y| {
                        (caret.x.ceil() as u32..(caret.x + caret.width).floor() as u32).any(|x| {
                            let pixel = pixels.pixel(x, y).unwrap();
                            pixel.red() < 200
                                && pixel.green() == 0
                                && pixel.blue() == 0
                                && pixel.alpha() == 255
                        })
                    });
                assert!(
                    ink,
                    "block glyph must remain visible even when foreground equals cursor color"
                );
            }

            assert!(draw(&mut renderer, &selected, Some(shape), true).is_empty());
        }
        let [block, line, underscore] = painted.as_slice() else {
            unreachable!()
        };
        assert_eq!(block.x, line.x);
        assert_eq!(block.y, line.y);
        assert_eq!(stock_caret.position(), line.position());
        assert!(block.width > line.width && block.height > underscore.height);
        assert_eq!(line.width, 2.0);
        assert_eq!(block.width, underscore.width);
        assert_eq!(block.y + block.height, underscore.y + underscore.height);
    }
}
