//! Scoped application text scaling in logical pixels.

use std::cell::Cell;

use crate::core::alignment;
use crate::core::text::{self, LineHeight, Span, Text, editor, highlighter};
use crate::core::{Font, Pixels, Point, Rectangle, Size};
use crate::graphics::text as graphics;

thread_local! {
    static SCALE: Cell<f32> = const { Cell::new(1.0) };
}

/// Runs a widget or overlay operation at the given application font scale.
pub fn with<T>(scale: f32, run: impl FnOnce() -> T) -> T {
    struct Restore(f32);
    impl Drop for Restore {
        fn drop(&mut self) {
            SCALE.set(self.0);
        }
    }
    let _restore = Restore(SCALE.replace(scale));
    run()
}

fn pixels(size: Pixels) -> Pixels {
    Pixels(size.0 * SCALE.get())
}

fn height(line_height: LineHeight) -> LineHeight {
    match line_height {
        LineHeight::Absolute(size) => LineHeight::Absolute(pixels(size)),
        relative => relative,
    }
}

/// Resolves a line height for a widget's layout before shaping its text.
pub fn line_height(line_height: LineHeight, size: Pixels) -> Pixels {
    height(line_height).to_absolute(pixels(size))
}

pub(crate) fn text<T, F>(mut text: Text<T, F>) -> Text<T, F> {
    text.size = pixels(text.size);
    text.line_height = height(text.line_height);
    text
}

/// A paragraph that inherits the scale of its widget's current layout scope.
#[derive(Clone, Debug, Default)]
pub struct Paragraph(pub(crate) graphics::Paragraph);

impl text::Paragraph for Paragraph {
    type Font = Font;

    fn with_text(content: Text<&str>) -> Self {
        Self(graphics::Paragraph::with_text(text(content)))
    }

    fn with_spans<Link>(content: Text<&[Span<'_, Link>]>) -> Self {
        if SCALE.get() == 1.0 {
            return Self(graphics::Paragraph::with_spans(content));
        }
        let spans: Vec<_> = content
            .content
            .iter()
            .map(|span| Span {
                text: span.text.clone(),
                size: span.size.map(pixels),
                line_height: span.line_height.map(height),
                font: span.font,
                color: span.color,
                link: span.link.as_ref(),
                highlight: span.highlight,
                padding: span.padding,
                underline: span.underline,
                strikethrough: span.strikethrough,
            })
            .collect();
        Self(graphics::Paragraph::with_spans(text(
            content.with_content(spans.as_slice()),
        )))
    }

    fn resize(&mut self, bounds: Size) {
        self.0.resize(bounds);
    }

    fn compare(&self, content: Text<()>) -> text::Difference {
        self.0.compare(text(content))
    }

    fn size(&self) -> Pixels {
        self.0.size()
    }

    fn font(&self) -> Font {
        self.0.font()
    }

    fn line_height(&self) -> LineHeight {
        self.0.line_height()
    }

    fn align_x(&self) -> text::Alignment {
        self.0.align_x()
    }

    fn align_y(&self) -> alignment::Vertical {
        self.0.align_y()
    }

    fn wrapping(&self) -> text::Wrapping {
        self.0.wrapping()
    }

    fn shaping(&self) -> text::Shaping {
        self.0.shaping()
    }

    fn bounds(&self) -> Size {
        self.0.bounds()
    }

    fn min_bounds(&self) -> Size {
        self.0.min_bounds()
    }

    fn hit_test(&self, point: Point) -> Option<text::Hit> {
        self.0.hit_test(point)
    }

    fn hit_span(&self, point: Point) -> Option<usize> {
        self.0.hit_span(point)
    }

    fn span_bounds(&self, index: usize) -> Vec<Rectangle> {
        self.0.span_bounds(index)
    }

    fn grapheme_position(&self, line: usize, index: usize) -> Option<Point> {
        self.0.grapheme_position(line, index)
    }
}

/// The text editor renderer with the surrounding widget's font scale.
#[derive(Debug, Default)]
pub struct Editor(pub(crate) graphics::Editor);

impl std::ops::Deref for Editor {
    type Target = graphics::Editor;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl text::Editor for Editor {
    type Font = Font;

    fn with_text(text: &str) -> Self {
        Self(graphics::Editor::with_text(text))
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn cursor(&self) -> editor::Cursor {
        self.0.cursor()
    }

    fn selection(&self) -> editor::Selection {
        self.0.selection()
    }

    fn copy(&self) -> Option<String> {
        self.0.copy()
    }

    fn line(&self, index: usize) -> Option<editor::Line<'_>> {
        self.0.line(index)
    }

    fn line_count(&self) -> usize {
        self.0.line_count()
    }

    fn perform(&mut self, action: editor::Action) {
        self.0.perform(action);
    }

    fn move_to(&mut self, cursor: editor::Cursor) {
        self.0.move_to(cursor);
    }

    fn bounds(&self) -> Size {
        self.0.bounds()
    }

    fn min_bounds(&self) -> Size {
        self.0.min_bounds()
    }

    fn update(
        &mut self,
        bounds: Size,
        font: Font,
        size: Pixels,
        line_height: LineHeight,
        wrapping: text::Wrapping,
        highlighter: &mut impl text::Highlighter,
    ) {
        self.0.update(
            bounds,
            font,
            pixels(size),
            height(line_height),
            wrapping,
            highlighter,
        );
    }

    fn highlight<H: text::Highlighter>(
        &mut self,
        font: Font,
        highlighter: &mut H,
        format: impl Fn(&H::Highlight) -> highlighter::Format<Font>,
    ) {
        self.0.highlight(font, highlighter, format);
    }
}
