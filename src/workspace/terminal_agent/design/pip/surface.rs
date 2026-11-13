//! Floating geometry and ordered pointer handling; the terminal keeps its full layout.

use std::time::{Duration, Instant};

use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, overlay, renderer};
use iced::{
    Element, Event, Length, Point, Rectangle, Renderer, Size, Theme, Vector, mouse, window,
};

const MARGIN: f32 = 12.0;
const SNAP_DURATION: Duration = Duration::from_millis(420);

pub(super) struct Options {
    pub(super) draggable: bool,
    pub(super) reduced_motion: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    #[default]
    BottomRight,
}

impl Corner {
    const ALL: [Self; 4] = [
        Self::TopLeft,
        Self::TopRight,
        Self::BottomLeft,
        Self::BottomRight,
    ];

    fn position(self, area: Size, panel: Size) -> Point {
        let left = MARGIN.min((area.width - panel.width).max(0.0) / 2.0);
        let top = MARGIN.min((area.height - panel.height).max(0.0) / 2.0);
        Point::new(
            if matches!(self, Self::TopLeft | Self::BottomLeft) {
                left
            } else {
                (area.width - panel.width - left).max(left)
            },
            if matches!(self, Self::TopLeft | Self::TopRight) {
                top
            } else {
                (area.height - panel.height - top).max(top)
            },
        )
    }

    fn nearest(point: Point, area: Size, panel: Size) -> Self {
        Self::ALL
            .into_iter()
            .min_by(|left, right| {
                left.position(area, panel)
                    .distance(point)
                    .total_cmp(&right.position(area, panel).distance(point))
            })
            .unwrap()
    }
}

pub(super) fn view<'a, M: 'a>(
    base: Element<'a, M>,
    panel: Element<'a, M>,
    run: window::Id,
    corner: Corner,
    size: Size,
    options: Options,
    snapped: impl Fn(Corner) -> M + 'a,
) -> Element<'a, M> {
    Element::new(Surface {
        children: [base, panel],
        run,
        corner,
        size,
        snapped: options
            .draggable
            .then(|| Box::new(snapped) as Box<dyn Fn(Corner) -> M>),
        reduced_motion: options.reduced_motion,
    })
}

pub(super) fn preview<'a, M: 'a>(base: Element<'a, M>, panel: Element<'a, M>) -> Element<'a, M> {
    Element::new(Surface {
        children: [base, panel],
        run: window::Id::unique(),
        corner: Corner::BottomRight,
        size: Size::new(360.0, 240.0),
        snapped: None,
        reduced_motion: true,
    })
}

struct Surface<'a, M> {
    children: [Element<'a, M>; 2],
    run: window::Id,
    corner: Corner,
    size: Size,
    snapped: Option<Box<dyn Fn(Corner) -> M + 'a>>,
    reduced_motion: bool,
}

struct Pointer {
    run: window::Id,
    last: Option<Point>,
    offset: Option<Vector>,
    position: Option<Point>,
    corner: Corner,
    node: layout::Node,
    area: Size,
    snap: Option<Snap>,
}

impl Pointer {
    fn new(run: window::Id, corner: Corner) -> Self {
        Self {
            run,
            last: None,
            offset: None,
            position: None,
            corner,
            node: layout::Node::default(),
            area: Size::ZERO,
            snap: None,
        }
    }

    fn clamp(&self, point: Point) -> Point {
        Point::new(
            point
                .x
                .clamp(0.0, (self.area.width - self.node.size().width).max(0.0)),
            point
                .y
                .clamp(0.0, (self.area.height - self.node.size().height).max(0.0)),
        )
    }

    fn move_to(&mut self, point: Point) {
        self.node.move_to_mut(self.clamp(point));
    }

    fn settle(&mut self) {
        self.snap = None;
        self.position = None;
        self.offset = None;
        self.move_to(self.corner.position(self.area, self.node.size()));
    }

    fn advance(&mut self, now: Instant) -> bool {
        let Some(snap) = &mut self.snap else {
            return false;
        };
        let start = *snap.start.get_or_insert(now);
        let elapsed = now.saturating_duration_since(start);
        if elapsed >= SNAP_DURATION {
            self.settle();
            return false;
        }
        // The Java reference uses an ease-out spring with this overshoot coefficient.
        let u = elapsed.as_secs_f32() / SNAP_DURATION.as_secs_f32() - 1.0;
        let spring = 1.0 + 2.70158 * u * u * u + 1.70158 * u * u;
        let point = snap.from + (snap.to - snap.from) * spring;
        self.position = Some(point);
        self.move_to(point);
        true
    }
}

struct Snap {
    from: Point,
    to: Point,
    start: Option<Instant>,
}

fn panel_layout(pointer: &Pointer, area: Rectangle) -> Layout<'_> {
    Layout::with_offset(Vector::new(area.x, area.y), &pointer.node)
}

impl<M> Widget<M, Theme, Renderer> for Surface<'_, M> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Pointer>()
    }
    fn state(&self) -> tree::State {
        tree::State::new(Pointer::new(self.run, self.corner))
    }
    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }
    fn diff(&self, tree: &mut Tree) {
        let pointer = tree.state.downcast_mut::<Pointer>();
        if pointer.run != self.run {
            *pointer = Pointer::new(self.run, self.corner);
        } else if self.corner != pointer.corner
            || self.reduced_motion && pointer.snap.is_some()
            || self.snapped.is_none() && (pointer.offset.is_some() || pointer.snap.is_some())
        {
            pointer.corner = self.corner;
            pointer.settle();
        }
        tree.diff_children(&self.children);
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
        let base = self.children[0]
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits);
        let area = base.size();
        let size = Size::new(
            self.size.width.min(area.width),
            self.size.height.min(area.height),
        );
        let mut panel = self.children[1].as_widget_mut().layout(
            &mut tree.children[1],
            renderer,
            &layout::Limits::new(size, size),
        );
        let pointer = tree.state.downcast_mut::<Pointer>();
        if pointer.area != area || pointer.node.size() != size {
            pointer.snap = None;
            pointer.position = None;
            pointer.offset = None;
        }
        pointer.area = area;
        let point = pointer
            .position
            .unwrap_or_else(|| pointer.corner.position(area, size));
        panel.move_to_mut(Point::new(
            point.x.clamp(0.0, (area.width - size.width).max(0.0)),
            point.y.clamp(0.0, (area.height - size.height).max(0.0)),
        ));
        pointer.node = panel.clone();
        layout::Node::with_children(area, vec![base, panel])
    }
    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.container(None, layout.bounds());
        let panel = panel_layout(tree.state.downcast_ref::<Pointer>(), layout.bounds());
        operation.traverse(&mut |operation| {
            for ((child, state), child_layout) in self
                .children
                .iter_mut()
                .zip(&mut tree.children)
                .zip([layout.child(0), panel])
            {
                child
                    .as_widget_mut()
                    .operate(state, child_layout, renderer, operation);
            }
        });
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
        let pointer = tree.state.downcast_mut::<Pointer>();
        let base = layout.child(0);
        let area = layout.bounds();
        if let Event::Window(window::Event::RedrawRequested(now)) = event
            && pointer.snap.is_some()
            && pointer.advance(*now)
        {
            shell.request_redraw_at(*now + Duration::from_millis(16));
        }
        if let Event::Mouse(mouse::Event::CursorMoved { position }) = event {
            pointer.last = Some(*position);
        }
        let cursor = if cursor.position().is_some() {
            pointer.last.map(mouse::Cursor::Available).unwrap_or(cursor)
        } else {
            cursor
        };
        if matches!(
            event,
            Event::Mouse(mouse::Event::CursorLeft) | Event::Window(window::Event::Unfocused)
        ) {
            pointer.last = None;
            if pointer.offset.is_some()
                || pointer.snap.is_some()
                    && matches!(event, Event::Window(window::Event::Unfocused))
            {
                pointer.settle();
                shell.request_redraw();
            }
        }
        let node = pointer.node.clone();
        let panel = Layout::with_offset(Vector::new(area.x, area.y), &node);
        if pointer.offset.is_some() {
            match event {
                Event::Mouse(mouse::Event::CursorMoved { position }) => {
                    let origin = *position - pointer.offset.unwrap();
                    pointer.position = Some(Point::new(origin.x - area.x, origin.y - area.y));
                    pointer.move_to(pointer.position.unwrap());
                    shell.request_redraw();
                    shell.capture_event();
                    return;
                }
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                    let point = pointer.position.take().unwrap_or(Point::new(
                        panel.bounds().x - area.x,
                        panel.bounds().y - area.y,
                    ));
                    pointer.offset = None;
                    if let Some(snapped) = &self.snapped {
                        let corner = Corner::nearest(point, area.size(), panel.bounds().size());
                        pointer.corner = corner;
                        shell.publish(snapped(corner));
                        let from = pointer.clamp(point);
                        let to = corner.position(area.size(), panel.bounds().size());
                        if !self.reduced_motion && from.distance(to) > 0.5 {
                            pointer.position = Some(from);
                            pointer.snap = Some(Snap {
                                from,
                                to,
                                start: None,
                            });
                        } else {
                            pointer.settle();
                        }
                    }
                    shell.request_redraw();
                    shell.capture_event();
                    return;
                }
                _ => {}
            }
        }
        self.children[1].as_widget_mut().update(
            &mut tree.children[1],
            event,
            panel,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
        if shell.is_event_captured() {
            return;
        }
        let hit = cursor.is_over(panel.bounds()) && cursor.is_over(*viewport);
        let grip = Rectangle {
            width: (panel.bounds().width - 150.0).max(0.0),
            height: 32.0,
            ..panel.bounds()
        };
        if self.snapped.is_some()
            && hit
            && cursor.is_over(grip)
            && matches!(
                event,
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            )
        {
            pointer.snap = None;
            pointer.offset = cursor
                .position()
                .map(|position| position - panel.bounds().position());
            pointer.position = Some(Point::new(
                panel.bounds().x - area.x,
                panel.bounds().y - area.y,
            ));
            shell.request_redraw();
            shell.capture_event();
            return;
        }
        // Empty panel space must not send mouse clicks or wheel movement to the terminal below.
        if hit && matches!(event, Event::Mouse(_) | Event::Touch(_)) {
            shell.capture_event();
            return;
        }
        self.children[0].as_widget_mut().update(
            &mut tree.children[0],
            event,
            base,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
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
        use iced::advanced::Renderer as _;
        let base = layout.child(0);
        let pointer = tree.state.downcast_ref::<Pointer>();
        let panel = panel_layout(pointer, layout.bounds());
        self.children[0].as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            base,
            if cursor.is_over(panel.bounds()) {
                mouse::Cursor::Unavailable
            } else {
                cursor
            },
            viewport,
        );
        if pointer.offset.is_some() {
            let area = layout.bounds();
            let selected = Corner::nearest(
                pointer.position.unwrap_or(Point::ORIGIN),
                area.size(),
                panel.bounds().size(),
            );
            for corner in Corner::ALL {
                let point = corner.position(area.size(), panel.bounds().size());
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: Rectangle::new(
                            Point::new(area.x + point.x, area.y + point.y),
                            panel.bounds().size(),
                        ),
                        border: iced::Border {
                            color: theme.palette().primary.scale_alpha(if corner == selected {
                                0.9
                            } else {
                                0.35
                            }),
                            width: 2.0,
                            radius: 8.0.into(),
                        },
                        ..renderer::Quad::default()
                    },
                    iced::Color::TRANSPARENT,
                );
            }
        }
        self.children[1].as_widget().draw(
            &tree.children[1],
            renderer,
            theme,
            style,
            panel,
            cursor,
            viewport,
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
        let base = layout.child(0);
        let panel = panel_layout(tree.state.downcast_ref::<Pointer>(), layout.bounds());
        if tree.state.downcast_ref::<Pointer>().offset.is_some() {
            return mouse::Interaction::Grabbing;
        }
        if cursor.is_over(panel.bounds()) {
            let interaction = self.children[1].as_widget().mouse_interaction(
                &tree.children[1],
                panel,
                cursor,
                viewport,
                renderer,
            );
            if self.snapped.is_some()
                && cursor.position().is_some_and(|p| {
                    p.y < panel.bounds().y + 32.0
                        && p.x < panel.bounds().x + panel.bounds().width - 150.0
                })
            {
                mouse::Interaction::Grab
            } else {
                interaction
            }
        } else {
            self.children[0].as_widget().mouse_interaction(
                &tree.children[0],
                base,
                cursor,
                viewport,
                renderer,
            )
        }
    }
    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'a, M, Theme, Renderer>> {
        let panel = panel_layout(tree.state.downcast_ref::<Pointer>(), layout.bounds());
        let overlays: Vec<_> = self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip([layout.child(0), panel])
            .filter_map(|((child, state), layout)| {
                child
                    .as_widget_mut()
                    .overlay(state, layout, renderer, viewport, translation)
            })
            .collect();
        (!overlays.is_empty()).then(|| overlay::Group::with_children(overlays).overlay())
    }
}

#[cfg(test)]
mod tests;
