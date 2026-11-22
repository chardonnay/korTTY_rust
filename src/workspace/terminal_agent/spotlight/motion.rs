//! Opening and submission transitions for the pane-local Spotlight card.

use std::time::Duration;

use iced::advanced::renderer::Renderer as _;
use iced::advanced::widget::{Operation, Tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, overlay, renderer};
use iced::widget::{container, opaque};
use iced::{
    Element, Event, Length, Rectangle, Renderer, Size, Theme, Transformation, Vector, mouse,
};

pub(super) const OPEN: Duration = Duration::from_millis(160);
const OPEN_FADE: Duration = Duration::from_millis(140);
const SUBMIT_FADE: Duration = Duration::from_millis(220);
const MORPH: Duration = Duration::from_millis(420);

#[derive(Clone, Copy)]
pub(in crate::workspace::terminal_agent) struct Frame {
    pub(super) opacity: f32,
    scale: f32,
    toward_status: f32,
}

impl Frame {
    pub(super) fn opening(age: Duration) -> Self {
        Self {
            opacity: ease(fraction(age, OPEN_FADE)),
            scale: 0.96 + 0.04 * ease(fraction(age, OPEN)),
            toward_status: 0.0,
        }
    }

    pub(super) fn submitted(age: Duration, into_pill: bool) -> Option<Self> {
        let duration = if into_pill { MORPH } else { SUBMIT_FADE };
        (age < duration).then(|| {
            let progress = fraction(age, duration);
            let travel = if into_pill { ease(progress) } else { 0.0 };
            Self {
                opacity: 1.0 - ease(progress),
                scale: 1.0 - 0.7 * travel,
                toward_status: travel,
            }
        })
    }

    fn transform(self, pane: Rectangle, card: Rectangle) -> Transformation {
        let origin = card.center();
        let target_x = pane.x + pane.width - 12.0 - card.width * 0.15;
        let target_y = pane.y + 8.0 + card.height * 0.15;
        Transformation::translate(
            origin.x + (target_x - origin.x) * self.toward_status,
            origin.y + (target_y - origin.y) * self.toward_status,
        ) * Transformation::scale(self.scale)
            * Transformation::translate(-origin.x, -origin.y)
    }
}

fn fraction(age: Duration, total: Duration) -> f32 {
    age.min(total).as_secs_f32() / total.as_secs_f32()
}

fn ease(value: f32) -> f32 {
    value * value * (3.0 - 2.0 * value)
}

pub(super) fn layer<'a, M: 'a>(
    card: Element<'a, M>,
    frame: Frame,
    interactive: bool,
) -> Element<'a, M> {
    let card = if interactive { opaque(card) } else { card };
    let center = container(card).center(Length::Fill).padding(8);
    if interactive && frame.scale == 1.0 {
        center.into()
    } else {
        Element::new(Layer {
            center: center.into(),
            frame,
            interactive,
            centered: true,
        })
    }
}

pub(in crate::workspace::terminal_agent) fn scale_card<'a, M: 'a>(
    card: Element<'a, M>,
    scale: f32,
) -> Element<'a, M> {
    if scale == 1.0 {
        return card;
    }
    Element::new(Layer {
        center: card,
        frame: Frame {
            opacity: 1.0,
            scale,
            toward_status: 0.0,
        },
        interactive: true,
        centered: false,
    })
}

struct Layer<'a, M> {
    center: Element<'a, M>,
    frame: Frame,
    interactive: bool,
    centered: bool,
}

impl<M> Layer<'_, M> {
    fn transform(&self, layout: Layout<'_>) -> Transformation {
        let outer = layout.bounds();
        let card = if self.centered {
            layout.child(0).bounds()
        } else {
            outer
        };
        self.frame.transform(outer, card)
    }
}

impl<M> Widget<M, Theme, Renderer> for Layer<'_, M> {
    fn tag(&self) -> iced::advanced::widget::tree::Tag {
        self.center.as_widget().tag()
    }

    fn state(&self) -> iced::advanced::widget::tree::State {
        self.center.as_widget().state()
    }

    fn children(&self) -> Vec<Tree> {
        self.center.as_widget().children()
    }

    fn diff(&self, tree: &mut Tree) {
        self.center.as_widget().diff(tree);
    }

    fn size(&self) -> Size<Length> {
        self.center.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.center.as_widget_mut().layout(tree, renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        if self.interactive {
            self.center
                .as_widget_mut()
                .operate(tree, layout, renderer, operation);
        }
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
        if self.interactive {
            let inverse = self.transform(layout).inverse();
            self.center.as_widget_mut().update(
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
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        if !self.interactive {
            return mouse::Interaction::None;
        }
        let inverse = self.transform(layout).inverse();
        self.center.as_widget().mouse_interaction(
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
        if self.interactive {
            self.center
                .as_widget_mut()
                .overlay(tree, layout, renderer, viewport, translation)
        } else {
            None
        }
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
        let Some(visible) = layout.bounds().intersection(viewport) else {
            return;
        };
        let transform = self.transform(layout);
        let inverse = transform.inverse();
        renderer.with_layer(visible, |renderer| {
            renderer.with_transformation(transform, |renderer| {
                self.center.as_widget().draw(
                    tree,
                    renderer,
                    theme,
                    style,
                    layout,
                    cursor * inverse,
                    &(visible * inverse),
                );
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_and_submit_have_distinct_geometry_and_deadlines() {
        let opening = Frame::opening(Duration::ZERO);
        assert_eq!(opening.opacity, 0.0);
        assert_eq!(opening.scale, 0.96);
        assert_eq!(Frame::opening(OPEN).scale, 1.0);
        let fade = Frame::submitted(SUBMIT_FADE / 2, false).unwrap();
        assert_eq!(
            (fade.opacity, fade.scale, fade.toward_status),
            (0.5, 1.0, 0.0)
        );
        assert!(Frame::submitted(SUBMIT_FADE, false).is_none());
        let morph = Frame::submitted(MORPH / 2, true).unwrap();
        let pane = Rectangle::with_size(Size::new(640.0, 420.0));
        let card = Rectangle {
            x: 90.0,
            y: 130.0,
            width: 460.0,
            height: 160.0,
        };
        let moved = card * morph.transform(pane, card);
        assert!(moved.center_x() > card.center_x());
        assert!(moved.center_y() < card.center_y());
        assert!(moved.width < card.width);
        assert!(Frame::submitted(MORPH, true).is_none());
    }
}
