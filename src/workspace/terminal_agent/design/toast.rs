//! Clipped motion for notices in the terminal Agent status panel.

use std::time::Duration;

use iced::advanced::renderer::Renderer as _;
use iced::advanced::widget::{Operation, Tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, overlay, renderer};
use iced::{Element, Event, Length, Point, Rectangle, Renderer, Size, Theme, Vector, mouse};

pub(super) const LIFETIME: Duration = Duration::from_secs(4);
const SLIDE: Duration = Duration::from_millis(260);
const APPEAR: Duration = Duration::from_millis(200);
const FADE: Duration = Duration::from_millis(220);
const COLLAPSE: Duration = Duration::from_millis(160);

#[derive(Clone, Copy)]
pub(super) struct Frame {
    pub(super) remaining: Duration,
    pub(super) opacity: f32,
    offset: f32,
    height: f32,
}

impl Frame {
    pub(super) fn at(age: Duration, reduced_motion: bool) -> Option<Self> {
        if reduced_motion {
            return (age < LIFETIME).then(|| Self {
                remaining: LIFETIME - age,
                ..Self::still()
            });
        }
        let after_slide = age.saturating_sub(SLIDE);
        let after_lifetime = after_slide.saturating_sub(LIFETIME);
        let collapse_age = after_lifetime.saturating_sub(FADE);
        (collapse_age < COLLAPSE).then(|| {
            let collapse = fraction(collapse_age, COLLAPSE);
            Self {
                remaining: LIFETIME.saturating_sub(after_slide),
                opacity: fraction(age, APPEAR) * (1.0 - fraction(after_lifetime, FADE)),
                offset: slide(age),
                height: 1.0 - collapse * collapse,
            }
        })
    }

    fn still() -> Self {
        Self {
            remaining: Duration::ZERO,
            opacity: 1.0,
            offset: 0.0,
            height: 1.0,
        }
    }
}

fn fraction(age: Duration, duration: Duration) -> f32 {
    age.min(duration).as_secs_f32() / duration.as_secs_f32()
}

fn slide(age: Duration) -> f32 {
    260.0 * (1.0 - fraction(age, SLIDE)).powi(3)
}

pub(super) fn review_frame(age: Duration, reduced_motion: bool) -> Frame {
    Frame {
        offset: if reduced_motion { 0.0 } else { slide(age) },
        ..Frame::still()
    }
}

pub(super) fn review_animating(age: Duration) -> bool {
    age < SLIDE
}

pub(super) fn view<'a, M: 'a>(content: impl Into<Element<'a, M>>, frame: Frame) -> Element<'a, M> {
    let content = content.into();
    if frame.offset == 0.0 && frame.height == 1.0 {
        content
    } else {
        Element::new(Card { content, frame })
    }
}

struct Card<'a, M> {
    content: Element<'a, M>,
    frame: Frame,
}

impl<M> Widget<M, Theme, Renderer> for Card<'_, M> {
    fn tag(&self) -> iced::advanced::widget::tree::Tag {
        self.content.as_widget().tag()
    }

    fn state(&self) -> iced::advanced::widget::tree::State {
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

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let mut content = self.content.as_widget_mut().layout(tree, renderer, limits);
        let size = content.size();
        content.move_to_mut(Point::new(self.frame.offset, 0.0));
        layout::Node::with_children(
            Size::new(size.width, size.height * self.frame.height),
            vec![content],
        )
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
            .operate(tree, layout.child(0), renderer, operation);
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
        let Some(visible) = layout.bounds().intersection(viewport) else {
            return;
        };
        let cursor = if self.frame.opacity > 0.0 && cursor.is_over(visible) {
            cursor
        } else {
            mouse::Cursor::Unavailable
        };
        self.content.as_widget_mut().update(
            tree,
            event,
            layout.child(0),
            cursor,
            renderer,
            clipboard,
            shell,
            &visible,
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
        layout
            .bounds()
            .intersection(viewport)
            .filter(|&visible| self.frame.opacity > 0.0 && cursor.is_over(visible))
            .map_or(mouse::Interaction::None, |visible| {
                self.content.as_widget().mouse_interaction(
                    tree,
                    layout.child(0),
                    cursor,
                    &visible,
                    renderer,
                )
            })
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
            .overlay(tree, layout.child(0), renderer, viewport, translation)
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
            renderer.with_layer(visible, |renderer| {
                self.content.as_widget().draw(
                    tree,
                    renderer,
                    theme,
                    style,
                    layout.child(0),
                    cursor,
                    &visible,
                );
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use iced::Font;
    use iced::advanced::clipboard;
    use iced::widget::button;
    use iced_runtime::{UserInterface, user_interface::Cache};

    use super::*;

    #[test]
    fn animated_notices_slide_count_down_fade_and_collapse() {
        let initial = Frame::at(Duration::ZERO, false).unwrap();
        assert_eq!(initial.offset, 260.0);
        assert_eq!(initial.opacity, 0.0);
        let steady = Frame::at(SLIDE, false).unwrap();
        assert_eq!(steady.offset, 0.0);
        assert_eq!(steady.opacity, 1.0);
        assert_eq!(steady.remaining, LIFETIME);
        let fading = Frame::at(SLIDE + LIFETIME + FADE / 2, false).unwrap();
        assert_eq!(fading.opacity, 0.5);
        assert_eq!(fading.height, 1.0);
        let collapsing = Frame::at(SLIDE + LIFETIME + FADE + COLLAPSE / 2, false).unwrap();
        assert_eq!(collapsing.opacity, 0.0);
        assert_eq!(collapsing.height, 0.75);
        assert!(Frame::at(SLIDE + LIFETIME + FADE + COLLAPSE, false).is_none());
    }

    #[test]
    fn reduced_motion_keeps_the_notice_still_for_four_seconds() {
        let still = Frame::at(Duration::ZERO, true).unwrap();
        assert_eq!(still.offset, 0.0);
        assert_eq!(still.opacity, 1.0);
        assert_eq!(still.height, 1.0);
        assert!(Frame::at(LIFETIME, true).is_none());
        assert_eq!(review_frame(Duration::ZERO, true).offset, 0.0);
        assert_eq!(review_frame(SLIDE, false).offset, 0.0);
    }

    #[test]
    fn entering_notice_only_accepts_clicks_in_the_visible_slice() {
        let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
        let frame = Frame::at(SLIDE / 2, false).unwrap();
        let toast = view(button("Open step").width(120).on_press(7), frame);
        let mut ui = UserInterface::build(
            toast,
            Size::new(300.0, 80.0),
            Cache::default(),
            &mut renderer,
        );
        assert!(frame.offset > 20.0);
        let events = [
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
        ];
        for (x, expected) in [(15.0, None), (140.0, None), (90.0, Some(7))] {
            let mut messages = Vec::new();
            ui.update(
                &events,
                mouse::Cursor::Available(Point::new(x, 12.0)),
                &mut renderer,
                &mut clipboard::Null,
                &mut messages,
            );
            assert_eq!(messages.as_slice(), expected.as_slice());
        }
    }
}
