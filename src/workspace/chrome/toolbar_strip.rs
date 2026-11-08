//! A single toolbar row that keeps a prefix of its measured buttons visible.

use iced::advanced::widget::{Operation, Tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, overlay, renderer};
use iced::widget::space;
use iced::{Element, Event, Length, Point, Rectangle, Renderer, Size, Theme, Vector, mouse};

use super::WorkspaceMessage;

pub(super) const TRIGGER_WIDTH: f32 = 34.0;
const SPACING: f32 = 6.0;

type Item<'a> = Element<'a, WorkspaceMessage>;

pub(super) fn strip<'a>(
    items: Vec<Item<'a>>,
    overflow: impl Fn(usize) -> Item<'a> + 'a,
) -> Item<'a> {
    Element::new(Strip {
        items,
        overflow: Box::new(overflow),
        trigger: space::horizontal().width(0).into(),
        visible: 0,
    })
}

struct Strip<'view> {
    items: Vec<Item<'view>>,
    overflow: Box<dyn Fn(usize) -> Item<'view> + 'view>,
    trigger: Item<'view>,
    visible: usize,
}

impl<'view> Strip<'view> {
    fn placed<'a>(
        &'a self,
        tree: &'a Tree,
        layout: Layout<'a>,
    ) -> impl Iterator<Item = ((&'a Item<'view>, &'a Tree), Layout<'a>)> {
        let trigger = (self.visible < self.items.len())
            .then(|| (&self.trigger, &tree.children[self.items.len()]));
        self.items
            .iter()
            .zip(&tree.children)
            .take(self.visible)
            .chain(trigger)
            .zip(layout.children())
    }

    fn placed_mut<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
    ) -> impl Iterator<Item = ((&'a mut Item<'view>, &'a mut Tree), Layout<'a>)> {
        let total = self.items.len();
        let (states, trigger_state) = tree.children.split_at_mut(total);
        let trigger = (self.visible < total).then_some((&mut self.trigger, &mut trigger_state[0]));
        self.items
            .iter_mut()
            .zip(states)
            .take(self.visible)
            .chain(trigger)
            .zip(layout.children())
    }
}

impl Widget<WorkspaceMessage, Theme, Renderer> for Strip<'_> {
    fn children(&self) -> Vec<Tree> {
        self.items
            .iter()
            .chain(std::iter::once(&self.trigger))
            .map(Tree::new)
            .collect()
    }

    fn diff(&self, tree: &mut Tree) {
        let children: Vec<_> = self
            .items
            .iter()
            .chain(std::iter::once(&self.trigger))
            .collect();
        tree.diff_children(&children);
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Shrink, Length::Shrink)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let available = limits.max().width.max(0.0);
        let measure =
            layout::Limits::new(Size::ZERO, Size::new(f32::INFINITY, limits.max().height));
        let mut nodes = Vec::new();
        let mut width = 0.0;
        for (item, state) in self.items.iter_mut().zip(&mut tree.children) {
            let node = item.as_widget_mut().layout(state, renderer, &measure);
            let next = width + spacing_after(&nodes) + node.size().width;
            if next > available {
                break;
            }
            width = next;
            nodes.push(node);
        }
        if nodes.len() < self.items.len() {
            while width + spacing_after(&nodes) + TRIGGER_WIDTH > available {
                let Some(last) = nodes.pop() else {
                    break;
                };
                width -= last.size().width + spacing_after(&nodes);
            }
            self.visible = nodes.len();
            self.trigger = (self.overflow)(self.visible);
            let state = &mut tree.children[self.items.len()];
            state.diff(&self.trigger);
            let trigger_limits = layout::Limits::new(
                Size::ZERO,
                Size::new(available.min(TRIGGER_WIDTH), limits.max().height),
            );
            let trigger = self
                .trigger
                .as_widget_mut()
                .layout(state, renderer, &trigger_limits);
            width += spacing_after(&nodes) + trigger.size().width;
            nodes.push(trigger);
        } else {
            self.visible = nodes.len();
        }
        let height = nodes
            .iter()
            .map(|node| node.size().height)
            .fold(0.0, f32::max);
        let mut x = 0.0;
        for node in &mut nodes {
            node.move_to_mut(Point::new(x, (height - node.size().height) / 2.0));
            x += node.size().width + SPACING;
        }
        layout::Node::with_children(
            limits.resolve(Length::Shrink, Length::Shrink, Size::new(width, height)),
            nodes,
        )
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.container(None, layout.bounds());
        operation.traverse(&mut |operation| {
            for ((item, state), bounds) in self.placed_mut(tree, layout) {
                item.as_widget_mut()
                    .operate(state, bounds, renderer, operation);
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
        shell: &mut Shell<'_, WorkspaceMessage>,
        viewport: &Rectangle,
    ) {
        for ((item, state), bounds) in self.placed_mut(tree, layout) {
            item.as_widget_mut().update(
                state, event, bounds, cursor, renderer, clipboard, shell, viewport,
            );
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
        for ((item, state), bounds) in self.placed(tree, layout) {
            if bounds.bounds().intersects(viewport) {
                item.as_widget()
                    .draw(state, renderer, theme, style, bounds, cursor, viewport);
            }
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
        self.placed(tree, layout)
            .map(|((item, state), bounds)| {
                item.as_widget()
                    .mouse_interaction(state, bounds, cursor, viewport, renderer)
            })
            .max()
            .unwrap_or_default()
    }

    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'a, WorkspaceMessage, Theme, Renderer>> {
        let overlays: Vec<_> = self
            .placed_mut(tree, layout)
            .filter_map(|((item, state), bounds)| {
                item.as_widget_mut()
                    .overlay(state, bounds, renderer, viewport, translation)
            })
            .collect();
        (!overlays.is_empty()).then(|| overlay::Group::with_children(overlays).overlay())
    }
}

fn spacing_after(nodes: &[layout::Node]) -> f32 {
    if nodes.is_empty() { 0.0 } else { SPACING }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use iced::{Font, Pixels};

    use super::*;

    #[test]
    fn toolbar_strip_reserves_the_trigger_only_when_a_measured_item_overflows() {
        let first_hidden = Cell::new(None);
        let items = [40.0, 60.0, 30.0]
            .into_iter()
            .map(|width| space::horizontal().width(width).height(32).into())
            .collect();
        let mut view = strip(items, |first| {
            first_hidden.set(Some(first));
            space::horizontal().width(TRIGGER_WIDTH).height(32).into()
        });
        let mut tree = Tree::new(&view);
        let renderer = Renderer::new(Font::DEFAULT, Pixels(14.0));
        let mut layout = |width| {
            first_hidden.set(None);
            view.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, Size::new(width, 200.0)),
            )
        };

        let wide = layout(142.0);
        assert_eq!(wide.children().len(), 3);
        assert_eq!(first_hidden.get(), None);
        assert_eq!(wide.size(), Size::new(142.0, 32.0));

        let narrow = layout(108.0);
        assert_eq!(first_hidden.get(), Some(1));
        assert_eq!(narrow.children().len(), 2);
        assert_eq!(narrow.children()[1].bounds().x, 46.0);
        assert_eq!(narrow.size(), Size::new(80.0, 32.0));

        let only_trigger = layout(37.0);
        assert_eq!(first_hidden.get(), Some(0));
        assert_eq!(only_trigger.children().len(), 1);
        assert_eq!(only_trigger.size(), Size::new(TRIGGER_WIDTH, 32.0));
    }
}
