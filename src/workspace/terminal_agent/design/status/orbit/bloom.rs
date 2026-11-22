//! Reveals the Orbit step list through an expanding circle centered on the ring.

use std::time::{Duration, Instant};

use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, overlay, renderer};
use iced::{
    Element, Event, Length, Point, Rectangle, Renderer, Size, Theme, Vector, mouse, window,
};

use super::{AgentMessage, BOTTOM, FRAME, Key, LIST_GAP, RIGHT, SIZE};

const OPEN: Duration = Duration::from_millis(340);
const CLOSE: Duration = Duration::from_millis(260);
const LIST_WIDTH: f32 = 320.0;

pub(super) struct Bloom<'a> {
    pub(super) steps: Element<'a, AgentMessage>,
    pub(super) footer: Element<'a, AgentMessage>,
    pub(super) key: Key,
    pub(super) open: bool,
    pub(super) reduced_motion: bool,
}

#[derive(Default)]
struct Motion {
    key: Option<Key>,
    open: bool,
    progress: f32,
    transition: Option<(f32, Instant)>,
}

impl Motion {
    fn redraw(&mut self, open: bool, immediate: bool, now: Instant) -> bool {
        if immediate {
            self.open = open;
            self.progress = if open { 1.0 } else { 0.0 };
            self.transition = None;
            return false;
        }
        let current = self.at(now);
        if self.open != open {
            self.open = open;
            self.transition = Some((current, now));
        }
        self.progress = self.at(now);
        let target = if open { 1.0 } else { 0.0 };
        if self.progress == target {
            self.transition = None;
        }
        self.transition.is_some()
    }

    fn at(&self, now: Instant) -> f32 {
        let target = if self.open { 1.0 } else { 0.0 };
        let Some((origin, start)) = self.transition else {
            return target;
        };
        let duration = if self.open { OPEN } else { CLOSE };
        let t =
            (now.saturating_duration_since(start).as_secs_f32() / duration.as_secs_f32()).min(1.0);
        if t == 1.0 {
            return target;
        }
        let eased = if self.open {
            opening_spline(t)
        } else if t < 0.2 {
            t * t / 0.36
        } else {
            (t - 0.1) / 0.9
        };
        origin + (target - origin) * eased
    }
}

fn opening_spline(t: f32) -> f32 {
    // Java's SPLINE(0.2, 0.9, 0.1, 1.0) maps time through the x component.
    let point = |u: f32, a: f32, b: f32| {
        let v = 1.0 - u;
        3.0 * v * v * u * a + 3.0 * v * u * u * b + u * u * u
    };
    let (mut min, mut max) = (0.0, 1.0);
    for _ in 0..14 {
        let u = (min + max) / 2.0;
        if point(u, 0.2, 0.1) < t {
            min = u;
        } else {
            max = u;
        }
    }
    point(min, 0.9, 1.0)
}

#[derive(Clone, Copy)]
struct Frame {
    progress: f32,
    center: Point,
    radius: f32,
}

impl Frame {
    fn visible_cursor(self, cursor: mouse::Cursor, bounds: Rectangle) -> mouse::Cursor {
        if self.progress > 0.0
            && cursor.position_over(bounds).is_some_and(|position| {
                self.progress == 1.0 || self.center.distance(position) <= self.radius
            })
        {
            cursor
        } else {
            mouse::Cursor::Unavailable
        }
    }
}

impl Bloom<'_> {
    fn frame(&self, tree: &Tree, pane: Rectangle, steps: Rectangle) -> Frame {
        let motion = tree.state.downcast_ref::<Motion>();
        let progress = if self.reduced_motion {
            if self.open { 1.0 } else { 0.0 }
        } else if motion.key == Some(self.key) {
            motion.progress
        } else {
            0.0
        };
        let seed = SIZE / 2.0;
        let center = Point::new(
            (pane.x + pane.width - RIGHT - seed).max(pane.x),
            (pane.y + pane.height - BOTTOM - seed).max(pane.y),
        );
        let reach = (center.x - steps.x)
            .abs()
            .max((center.x - steps.x - steps.width).abs())
            .hypot(
                (center.y - steps.y)
                    .abs()
                    .max((center.y - steps.y - steps.height).abs()),
            );
        Frame {
            progress,
            center,
            radius: seed + progress * (reach - seed).max(0.0),
        }
    }
}

impl Widget<AgentMessage, Theme, Renderer> for Bloom<'_> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Motion>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(Motion::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.steps), Tree::new(&self.footer)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&[&self.steps, &self.footer]);
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let size = limits.resolve(Length::Fill, Length::Fill, Size::ZERO);
        let bottom = (size.height - BOTTOM - SIZE - LIST_GAP).max(0.0);
        let right = (size.width - RIGHT).max(0.0);
        let width = (right - 8.0).clamp(0.0, LIST_WIDTH);
        let footer = self.footer.as_widget_mut().layout(
            &mut tree.children[1],
            renderer,
            &layout::Limits::new(Size::ZERO, Size::new(width, bottom)),
        );
        let footer_size = footer.size();
        let list_bottom = (bottom
            - footer_size.height
            - if footer_size.height > 0.0 {
                LIST_GAP
            } else {
                0.0
            })
        .max(0.0);
        let list = self.steps.as_widget_mut().layout(
            &mut tree.children[0],
            renderer,
            &layout::Limits::new(Size::ZERO, Size::new(width, list_bottom)),
        );
        let list_size = list.size();
        layout::Node::with_children(
            size,
            vec![
                list.move_to(Point::new(
                    right - list_size.width,
                    list_bottom - list_size.height,
                )),
                footer.move_to(Point::new(
                    right - footer_size.width,
                    bottom - footer_size.height,
                )),
            ],
        )
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, AgentMessage>,
        viewport: &Rectangle,
    ) {
        if let Event::Window(window::Event::RedrawRequested(now)) = event {
            let motion = tree.state.downcast_mut::<Motion>();
            if motion.key != Some(self.key) {
                *motion = Motion {
                    key: Some(self.key),
                    ..Motion::default()
                };
                tree.children[0] = Tree::new(&self.steps);
            } else if motion.open && !self.open {
                tree.children[0] = Tree::new(&self.steps);
            }
            if motion.redraw(self.open, self.reduced_motion, *now) {
                shell.request_redraw_at(*now + FRAME);
            }
        }
        let mut children = layout.children();
        let steps = children.next().unwrap();
        let footer = children.next().unwrap();
        let frame = self.frame(tree, layout.bounds(), steps.bounds());
        let step_cursor = steps
            .bounds()
            .intersection(viewport)
            .map_or(mouse::Cursor::Unavailable, |visible| {
                frame.visible_cursor(cursor, visible)
            });
        if self.open
            && frame.progress > 0.0
            && (frame.progress == 1.0 || !matches!(event, Event::Keyboard(_)))
        {
            self.steps.as_widget_mut().update(
                &mut tree.children[0],
                event,
                steps,
                step_cursor,
                renderer,
                clipboard,
                shell,
                viewport,
            );
        } else if step_cursor.position().is_some()
            && matches!(
                event,
                Event::Mouse(mouse::Event::ButtonPressed(_))
                    | Event::Touch(iced::touch::Event::FingerPressed { .. })
            )
        {
            shell.capture_event();
        }
        if !shell.is_event_captured() {
            self.footer.as_widget_mut().update(
                &mut tree.children[1],
                event,
                footer,
                cursor,
                renderer,
                clipboard,
                shell,
                viewport,
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
        let mut children = layout.children();
        let steps = children.next().unwrap();
        let footer = children.next().unwrap();
        let frame = self.frame(tree, layout.bounds(), steps.bounds());
        if steps
            .bounds()
            .intersection(viewport)
            .is_some_and(|visible| frame.visible_cursor(cursor, visible).position().is_some())
        {
            if self.open {
                self.steps.as_widget().mouse_interaction(
                    &tree.children[0],
                    steps,
                    cursor,
                    viewport,
                    renderer,
                )
            } else {
                mouse::Interaction::Idle
            }
        } else {
            self.footer.as_widget().mouse_interaction(
                &tree.children[1],
                footer,
                cursor,
                viewport,
                renderer,
            )
        }
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        let mut children = layout.children();
        let steps = children.next().unwrap();
        let footer = children.next().unwrap();
        if self.open && self.frame(tree, layout.bounds(), steps.bounds()).progress == 1.0 {
            self.steps
                .as_widget_mut()
                .operate(&mut tree.children[0], steps, renderer, operation);
        }
        self.footer
            .as_widget_mut()
            .operate(&mut tree.children[1], footer, renderer, operation);
    }

    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'a, AgentMessage, Theme, Renderer>> {
        let mut children = layout.children();
        let steps = children.next().unwrap();
        let footer = children.next().unwrap();
        let show_steps =
            self.open && self.frame(tree, layout.bounds(), steps.bounds()).progress == 1.0;
        let [step_tree, footer_tree] = tree.children.as_mut_slice() else {
            unreachable!()
        };
        let overlays = [
            self.footer.as_widget_mut().overlay(
                footer_tree,
                footer,
                renderer,
                viewport,
                translation,
            ),
            show_steps
                .then(|| {
                    self.steps.as_widget_mut().overlay(
                        step_tree,
                        steps,
                        renderer,
                        viewport,
                        translation,
                    )
                })
                .flatten(),
        ];
        let overlays: Vec<_> = overlays.into_iter().flatten().collect();
        (!overlays.is_empty()).then(|| overlay::Group::with_children(overlays).overlay())
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
        let mut children = layout.children();
        let steps = children.next().unwrap();
        let footer = children.next().unwrap();
        self.footer.as_widget().draw(
            &tree.children[1],
            renderer,
            theme,
            style,
            footer,
            cursor,
            viewport,
        );
        let frame = self.frame(tree, layout.bounds(), steps.bounds());
        let Some(visible) = steps
            .bounds()
            .intersection(viewport)
            .filter(|_| frame.progress > 0.0)
        else {
            return;
        };
        let cursor = if self.open {
            frame.visible_cursor(cursor, visible)
        } else {
            mouse::Cursor::Unavailable
        };
        let draw_steps = |renderer: &mut Renderer| {
            self.steps.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                steps,
                cursor,
                &visible,
            );
        };
        if frame.progress == 1.0 {
            draw_steps(renderer);
        } else {
            renderer.with_circle_clip(visible, frame.center, frame.radius, draw_steps);
        }
    }
}

#[cfg(test)]
mod tests;
