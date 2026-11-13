use std::fmt;

use iced::advanced::text::Highlighter;
use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, renderer};
use iced::widget::TextEditor;
use iced::{Element, Event, Length, Rectangle, Renderer, Size, Theme, mouse};
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

pub(crate) fn view<'a, H: Highlighter, Message: 'a>(
    editor: TextEditor<'a, H, Message>,
    content_theme: Theme,
    appearance: Appearance,
    font_size: u8,
    preferences: Preferences,
) -> Element<'a, Message> {
    let editor = editor
        .font(appearance.font)
        .size(f32::from(font_size))
        .padding(PADDING)
        .style(move |_, status| iced::widget::text_editor::Style {
            value: TextEditorCaret::VALUE_COLOR,
            ..appearance.style(&content_theme, status)
        });
    Element::new(CaretEditor {
        editor: editor.into(),
        caret: TextEditorCaret {
            foreground: appearance.foreground.color(),
            color: preferences.color.color(),
            shape: preferences.shape.renderer_shape(),
            clip_bounds: None,
            fallback_cell_width: cell_width_ratio(appearance.font) * f32::from(font_size),
        },
    })
}

struct CaretEditor<'a, Message> {
    editor: Element<'a, Message>,
    caret: TextEditorCaret,
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
            clip_bounds: layout.bounds().shrink(PADDING).intersection(viewport),
            ..self.caret
        };
        renderer.with_text_editor_caret(caret, |renderer| {
            self.editor
                .as_widget()
                .draw(tree, renderer, theme, style, layout, cursor, viewport);
        });
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
    use iced::advanced::renderer::Renderer as _;
    use iced::advanced::widget::operation::focusable;
    use iced::widget::{self, text_editor};
    use iced::{Background, Color, Font, Pixels};

    use super::*;

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
