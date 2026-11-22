//! Composites status transitions without changing the terminal layout.

use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, overlay, renderer};
use iced::{Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector, mouse, window};

mod backdrop;
pub(super) use backdrop::{Backdrop, CORNER_RADIUS};
mod entrance;
pub(super) use entrance::Entrance;

pub(super) fn view<'a, M: 'a>(content: Element<'a, M>, opacity: f32) -> Element<'a, M> {
    if opacity == 1.0 {
        content
    } else {
        Element::new(Fade {
            content,
            opacity,
            frost: None,
        })
    }
}

pub(super) fn frosted<'a, M: 'a>(
    content: Element<'a, M>,
    opacity: f32,
    backdrop: &'a Backdrop,
    entrance: &'a Entrance,
) -> Element<'a, M> {
    Element::new(Fade {
        content,
        opacity,
        frost: Some(Frost { backdrop, entrance }),
    })
}

struct Frost<'a> {
    backdrop: &'a Backdrop,
    entrance: &'a Entrance,
}

struct Fade<'a, M> {
    content: Element<'a, M>,
    opacity: f32,
    frost: Option<Frost<'a>>,
}

impl<M> Fade<'_, M> {
    fn frame(&self) -> entrance::Frame {
        self.frost
            .as_ref()
            .map_or(entrance::Frame::SHOWN, |frost| frost.entrance.frame())
    }
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
        if let Event::Window(window::Event::RedrawRequested(now)) = event
            && let Some(frost) = &self.frost
            && let Some(next) = frost.entrance.redraw(*now)
        {
            shell.request_redraw_at(next);
        }
        let inverse = self.frame().transform(layout.bounds()).inverse();
        self.content.as_widget_mut().update(
            tree,
            event,
            layout,
            cursor * inverse,
            renderer,
            clipboard,
            shell,
            &(*viewport * inverse),
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
        let inverse = self.frame().transform(layout.bounds()).inverse();
        self.content.as_widget().mouse_interaction(
            tree,
            layout,
            cursor * inverse,
            &(*viewport * inverse),
            renderer,
        )
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
        use iced::advanced::{Renderer as _, image::Renderer as _};

        let frame = self.frame();
        let opacity = self.opacity * frame.opacity;
        let bounds = layout.bounds();
        let transform = frame.transform(bounds);
        let Some(visible) = (bounds * transform)
            .intersection(viewport)
            .filter(|_| opacity > 0.0)
        else {
            return;
        };
        let glass = self.frost.as_ref().and_then(|frost| {
            frost.backdrop.image(
                renderer,
                bounds,
                bounds.intersection(viewport)?,
                theme.palette().background,
            )
        });
        let inverse = transform.inverse();
        let local_clip = visible * inverse;
        let local_viewport = *viewport * inverse;
        let draw_card = |renderer: &mut Renderer| {
            self.content.as_widget().draw(
                tree,
                renderer,
                theme,
                style,
                layout,
                cursor * inverse,
                &local_viewport,
            );
        };
        renderer.with_opacity(visible, opacity, |renderer| {
            renderer.with_transformation(transform, |renderer| {
                if let Some(image) = glass {
                    renderer.with_layer(local_clip, |renderer| {
                        renderer.draw_image(image, bounds, local_clip)
                    });
                    renderer.with_layer(local_clip, draw_card);
                } else {
                    draw_card(renderer);
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::advanced::{Renderer as _, clipboard, graphics::Viewport, renderer::Quad};
    use iced::widget::{container, row, space, stack};
    use iced::{Color, Font};
    use iced_runtime::{UserInterface, user_interface::Cache};
    use std::time::{Duration, Instant};

    #[test]
    fn hud_entrance_repaints_the_scaled_card_without_relayout() {
        let backdrop = Backdrop::default();
        let entrance = Entrance::default();
        let scene = Size::new(160.0, 84.0);
        let dimensions = Size::new(160, 84);
        let surface = Rectangle::with_size(scene);
        let cursor = mouse::Cursor::Unavailable;
        let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
        let white_card: Element<'_, ()> = container(space())
            .width(100)
            .height(60)
            .style(|_| container::Style::default().background(Color::WHITE))
            .into();
        let content =
            container(frosted(white_card, 1.0, &backdrop, &entrance)).center(Length::Fill);
        let mut ui = UserInterface::build(content, scene, Cache::default(), &mut renderer);
        let mut pixmap = tiny_skia::Pixmap::new(dimensions.width, dimensions.height).unwrap();
        let mut mask = tiny_skia::Mask::new(dimensions.width, dimensions.height).unwrap();
        let start = Instant::now();
        let mut render = |ms| {
            let now = start + Duration::from_millis(ms);
            let (state, _) = ui.update(
                &[Event::Window(window::Event::RedrawRequested(now))],
                cursor,
                &mut renderer,
                &mut clipboard::Null,
                &mut Vec::new(),
            );
            assert!(!state.has_layout_changed());
            ui.draw(
                &mut renderer,
                &Theme::Dark,
                &renderer::Style::default(),
                cursor,
            );
            renderer.draw(
                &mut pixmap.as_mut(),
                &mut mask,
                &Viewport::with_physical_size(dimensions, 1.0),
                &[surface],
                Color::BLACK,
            );
            (
                pixmap.pixel(80, 42).unwrap().red(),
                pixmap.pixel(30, 42).unwrap().red(),
            )
        };
        assert_eq!(render(0), (0, 0));
        let (center, edge) = render(80);
        assert!((25..180).contains(&center), "{center}");
        assert_eq!(edge, 0);
        assert_eq!(render(240), (255, 255));
    }

    #[test]
    fn frosted_card_blurs_the_pixels_beneath_it_and_keeps_its_tint_above_them() {
        let backdrop = Backdrop::default();
        let entrance = Entrance::default();
        entrance.settle();
        let size = Size::new(160.0, 84.0);
        let dimensions = Size::new(160, 84);
        let surface = Rectangle::with_size(size);
        let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
        let right = container(space())
            .width(80)
            .height(Length::Fill)
            .style(|_| container::Style::default().background(Color::WHITE));
        let card = container(space()).width(100).height(60).style(|_| {
            container::Style::default()
                .background(Color::from_rgba(0.0, 1.0, 0.0, 0.25))
                .border(iced::Border {
                    radius: CORNER_RADIUS.into(),
                    ..Default::default()
                })
        });
        let card: Element<'_, ()> = card.into();
        let content = stack![
            row![space::horizontal().width(80), right],
            container(frosted(card, 0.5, &backdrop, &entrance)).center(Length::Fill),
        ];
        let mut ui = UserInterface::build(content, size, Cache::default(), &mut renderer);
        ui.draw(
            &mut renderer,
            &Theme::Dark,
            &renderer::Style::default(),
            mouse::Cursor::Unavailable,
        );
        let mut pixmap = tiny_skia::Pixmap::new(dimensions.width, dimensions.height).unwrap();
        let mut mask = tiny_skia::Mask::new(dimensions.width, dimensions.height).unwrap();
        renderer.draw(
            &mut pixmap.as_mut(),
            &mut mask,
            &Viewport::with_physical_size(dimensions, 1.0),
            &[surface],
            Color::BLACK,
        );
        let blurred = pixmap.pixel(78, 42).unwrap();
        assert!(blurred.red() > 20 && blurred.red() < 95, "{blurred:?}");
        assert!(blurred.green() > blurred.red(), "{blurred:?}");
        assert_eq!(pixmap.pixel(78, 8).unwrap().red(), 0);
        assert_eq!(pixmap.pixel(82, 8).unwrap().red(), 255);
        assert_eq!(pixmap.pixel(30, 12).unwrap().red(), 0);
        assert_eq!(pixmap.pixel(30, 12).unwrap().green(), 0);
    }

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
