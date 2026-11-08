use iced::advanced::widget::tree::{self, Tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, mouse, renderer};
use iced::widget::{rich_text, text};
use iced::{Element, Event, Font, Length, Rectangle, Renderer, Size, Theme};

pub(in crate::workspace) fn journal_rich_text<'a, Key, Link, Message>(
    key: Key,
    spans: Vec<text::Span<'a, Link, Font>>,
    font: Font,
    size: f32,
    on_link: impl Fn(Link) -> Message + 'a,
) -> Element<'a, Message>
where
    Key: Clone + Eq + 'static,
    Link: Clone + Eq + 'static,
    Message: 'a,
{
    let targets = spans.iter().map(|span| span.link.clone()).collect();
    Element::new(LinkText {
        identity: Identity { key, targets },
        content: rich_text(spans)
            .font(font)
            .size(size)
            .on_link_click(on_link)
            .into(),
    })
}

#[derive(Clone, PartialEq, Eq)]
struct Identity<Key, Link> {
    key: Key,
    targets: Vec<Option<Link>>,
}

struct LinkText<'a, Key, Link, Message> {
    identity: Identity<Key, Link>,
    content: Element<'a, Message>,
}

impl<Key, Link, Message> Widget<Message, Theme, Renderer> for LinkText<'_, Key, Link, Message>
where
    Key: Clone + Eq + 'static,
    Link: Clone + Eq + 'static,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Identity<Key, Link>>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(self.identity.clone())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(self.content.as_widget())]
    }

    fn diff(&self, tree: &mut Tree) {
        let identity = tree.state.downcast_mut::<Identity<Key, Link>>();
        if *identity != self.identity {
            // Rich stores only a pressed span index. The index may refer to a
            // different link after a live card or the selected entry changes.
            identity.clone_from(&self.identity);
            tree.children[0] = Tree::new(self.content.as_widget());
        } else {
            tree.children[0].diff(self.content.as_widget());
        }
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
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
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
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
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
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
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
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }
}

#[cfg(test)]
mod tests {
    use iced::advanced::clipboard;
    use iced::widget::span;
    use iced::{Pixels, Point};

    use super::*;

    #[test]
    fn journal_link_release_cannot_activate_a_replaced_span() {
        let renderer = Renderer::new(Font::DEFAULT, Pixels(13.0));
        let limits = layout::Limits::new(Size::ZERO, Size::new(200.0, 50.0));
        let view = |key, target| {
            journal_rich_text(
                key,
                vec![span("Link").link(target)],
                Font::DEFAULT,
                13.0,
                |link| link,
            )
        };
        let mut first = view(1, "first");
        let mut tree = Tree::new(first.as_widget());
        let event = |widget: &mut Element<'_, &'static str>, tree: &mut Tree, pressed: bool| {
            let node = widget.as_widget_mut().layout(tree, &renderer, &limits);
            let bounds = node.bounds();
            let cursor = mouse::Cursor::Available(Point::new(4.0, bounds.height / 2.0));
            let mouse_event = if pressed {
                mouse::Event::ButtonPressed(mouse::Button::Left)
            } else {
                mouse::Event::ButtonReleased(mouse::Button::Left)
            };
            let mut messages = Vec::new();
            let mut shell = Shell::new(&mut messages);
            widget.as_widget_mut().update(
                tree,
                &Event::Mouse(mouse_event),
                Layout::new(&node),
                cursor,
                &renderer,
                &mut clipboard::Null,
                &mut shell,
                &bounds,
            );
            if pressed {
                assert!(shell.is_event_captured(), "press must hit the link");
            }
            messages
        };

        assert!(event(&mut first, &mut tree, true).is_empty());
        let mut same = view(1, "first");
        tree.diff(same.as_widget());
        assert_eq!(event(&mut same, &mut tree, false), ["first"]);

        for (key, target) in [(2, "first"), (1, "second")] {
            tree.diff(first.as_widget());
            assert!(event(&mut first, &mut tree, true).is_empty());
            let mut replacement = view(key, target);
            tree.diff(replacement.as_widget());
            assert!(event(&mut replacement, &mut tree, false).is_empty());
            assert!(event(&mut replacement, &mut tree, true).is_empty());
            assert_eq!(event(&mut replacement, &mut tree, false), [target]);
        }
    }
}
