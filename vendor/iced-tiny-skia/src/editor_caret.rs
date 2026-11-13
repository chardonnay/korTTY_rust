use crate::Renderer;
use crate::core::Renderer as _;
use crate::core::text::editor::{Editor as _, Selection};
use crate::core::{Color, Point, Rectangle, renderer};
use crate::graphics::text::{Editor, editor};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextEditorCaretShape {
    Block,
    Line,
    Underline,
}

/// Appearance scoped to one iced TextEditor draw.
#[derive(Debug, Clone, Copy)]
pub struct TextEditorCaret {
    pub foreground: Color,
    pub color: Color,
    pub shape: TextEditorCaretShape,
    pub clip_bounds: Option<Rectangle>,
    pub fallback_cell_width: f32,
}

impl TextEditorCaret {
    // TextEditor uses Style::value for both fill_editor and the blinking
    // caret quad. The adapter assigns this color only inside the draw scope.
    pub const VALUE_COLOR: Color = Color {
        r: 0.37,
        g: 0.43,
        b: 0.51,
        a: 0.0,
    };
}

#[derive(Debug)]
pub(crate) struct Scope {
    appearance: TextEditorCaret,
    text: Option<(editor::Weak, Point)>,
}

impl Scope {
    pub(crate) fn record_text(&mut self, editor: &Editor, origin: Point) -> Color {
        self.text = Some((editor.downgrade(), origin));
        self.appearance.foreground
    }

    fn paint(&self, quad: renderer::Quad) -> Option<Paint> {
        let clip_bounds = self.appearance.clip_bounds?;
        let text = self
            .text
            .as_ref()
            .and_then(|(weak, origin)| Some((weak.upgrade()?, *origin)));
        let mut bounds = quad.bounds;
        bounds.width = self.appearance.fallback_cell_width;
        if let Some((editor, origin)) = &text
            && let Selection::Caret(position) = editor.selection()
        {
            let buffer = editor.buffer();
            bounds.x = origin.x + position.x;
            bounds.y = origin.y + position.y;
            bounds.height = buffer.metrics().line_height;
            if let Some(run) = buffer
                .layout_runs()
                .find(|run| (run.line_top - position.y).abs() < 0.5)
                && let Some(glyph) = run.glyphs.iter().find(|glyph| {
                    glyph.x <= position.x + 0.1 && glyph.x + glyph.w > position.x + 0.1
                })
            {
                bounds.width = (glyph.x + glyph.w - position.x).max(1.0);
            }
        }
        match self.appearance.shape {
            TextEditorCaretShape::Block => {}
            TextEditorCaretShape::Line => bounds.width = 2.0,
            TextEditorCaretShape::Underline => {
                let height = bounds.height.min(2.0);
                bounds.y += bounds.height - height;
                bounds.height = height;
            }
        }
        let bounds = bounds.intersection(&clip_bounds)?;
        Some(Paint {
            quad: renderer::Quad { bounds, ..quad },
            color: self.appearance.color,
            glyph: text.filter(|_| self.appearance.shape == TextEditorCaretShape::Block),
        })
    }
}

struct Paint {
    quad: renderer::Quad,
    color: Color,
    glyph: Option<(Editor, Point)>,
}

impl Renderer {
    pub fn with_text_editor_caret(
        &mut self,
        appearance: TextEditorCaret,
        draw: impl FnOnce(&mut Self),
    ) {
        let previous = self.editor_caret.replace(Scope {
            appearance,
            text: None,
        });
        draw(self);
        self.editor_caret = previous;
    }

    pub(crate) fn paint_editor_caret(&mut self, quad: renderer::Quad) {
        let Some(paint) = self
            .editor_caret
            .as_ref()
            .and_then(|scope| scope.paint(quad))
        else {
            return;
        };
        if let Some((editor, origin)) = paint.glyph {
            let bounds = paint.quad.bounds;
            // The block covers the original glyph. Paint the same shaped glyph
            // with contrasting ink inside the block without erasing its neighbors.
            let ink = if paint.color.relative_luminance() > 0.179 {
                Color::BLACK
            } else {
                Color::WHITE
            };
            self.with_layer(bounds, |renderer| {
                let (layer, transformation) = renderer.layers.current_mut();
                layer.draw_quad(paint.quad, paint.color.into(), transformation);
                layer.draw_editor(&editor, origin, ink, bounds, transformation);
            });
        } else {
            let (layer, transformation) = self.layers.current_mut();
            layer.draw_quad(paint.quad, paint.color.into(), transformation);
        }
    }
}
