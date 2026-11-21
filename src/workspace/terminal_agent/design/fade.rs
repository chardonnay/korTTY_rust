//! Composites the timed status fade without changing the panel's layout or input.

use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, overlay, renderer};
use iced::{Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector, mouse};

pub(super) fn view<'a, M: 'a>(content: Element<'a, M>, opacity: f32) -> Element<'a, M> {
    if opacity == 1.0 {
        content
    } else {
        Element::new(Fade { content, opacity })
    }
}

struct Fade<'a, M> {
    content: Element<'a, M>,
    opacity: f32,
}

impl<M> Widget<M, Theme, Renderer> for Fade<'_, M> {
    fn tag(&self) -> tree::Tag {
        self.content.as_widget().tag()
    }

    fn state(&self) -> tree::State {
        self.content.as_widget().state()
    }

    fn children(&self) -> Vec<Tree> {
        self.content.as_widget().children()
    }

    fn diff(&self, tree: &mut Tree) {
        self.content.as_widget().diff(tree);
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content.as_widget_mut().layout(tree, renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
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
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(
            tree, event, layout, cursor, renderer, clipboard, shell, viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content
            .as_widget()
            .mouse_interaction(tree, layout, cursor, viewport, renderer)
    }

    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'a, M, Theme, Renderer>> {
        self.content
            .as_widget_mut()
            .overlay(tree, layout, renderer, viewport, translation)
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
        if let Some(visible) = layout.bounds().intersection(viewport) {
            renderer.with_opacity(visible, self.opacity, |renderer| {
                self.content
                    .as_widget()
                    .draw(tree, renderer, theme, style, layout, cursor, viewport);
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::advanced::{Renderer as _, graphics::Viewport, renderer::Quad};
    use iced::{Color, Font};

    #[test]
    fn status_fade_composites_overlapping_content_once_inside_the_scaled_clip() {
        let dimensions = Size::new(80, 64);
        let viewport = Viewport::with_physical_size(dimensions, 2.0);
        let surface = Rectangle::with_size(viewport.logical_size());
        let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
        let draw = |renderer: &mut Renderer, opacity| {
            renderer.reset(surface);
            renderer.with_translation(Vector::new(6.0, 4.0), |renderer| {
                renderer.with_layer(
                    Rectangle {
                        x: 1.0,
                        y: 1.0,
                        width: 18.0,
                        height: 16.0,
                    },
                    |renderer| {
                        renderer.with_opacity(
                            Rectangle {
                                x: 3.0,
                                y: 2.0,
                                width: 21.0,
                                height: 13.0,
                            },
                            opacity,
                            |renderer| {
                                renderer.fill_quad(
                                    Quad {
                                        bounds: surface,
                                        ..Quad::default()
                                    },
                                    Color::WHITE,
                                );
                                renderer.fill_quad(
                                    Quad {
                                        bounds: Rectangle {
                                            x: 8.0,
                                            y: 4.0,
                                            width: 5.0,
                                            height: 5.0,
                                        },
                                        ..Quad::default()
                                    },
                                    Color::BLACK,
                                );
                            },
                        );
                    },
                );
            });
        };
        draw(&mut renderer, 0.5);
        let layers = renderer.layers().to_vec();
        let mut pixmap = tiny_skia::Pixmap::new(dimensions.width, dimensions.height).unwrap();
        let mut mask = tiny_skia::Mask::new(dimensions.width, dimensions.height).unwrap();
        renderer.draw(
            &mut pixmap.as_mut(),
            &mut mask,
            &viewport,
            &[surface],
            Color::BLACK,
        );
        let pixel = |x, y| pixmap.pixel(x, y).unwrap().red();
        assert!(pixel(20, 14).abs_diff(128) <= 1);
        assert_eq!(pixel(30, 20), 0);
        assert_eq!(pixel(16, 14), 0);
        assert_eq!(pixel(51, 14), 0);

        draw(&mut renderer, 0.25);
        assert!(layers.iter().zip(renderer.layers()).any(|(before, after)| {
            iced_tiny_skia::Layer::damage(before, after).contains(&Rectangle {
                x: 9.0,
                y: 6.0,
                width: 16.0,
                height: 13.0,
            })
        }));
    }
}
