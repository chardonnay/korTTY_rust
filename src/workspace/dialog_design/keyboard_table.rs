//! Keyboard navigation scoped to a focused table.

use iced::advanced::renderer::Renderer as _;
use iced::advanced::widget::{Operation, Tree, operation::Focusable, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, overlay, renderer};
use iced::keyboard::{self, Key, key::Named};
use iced::{
    Color, Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector, mouse, window,
};

pub(crate) enum TableKey<Message> {
    Ignored,
    Handled(Option<Message>),
}

pub(crate) fn view<'a, Message: 'a>(
    content: Element<'a, Message>,
    on_key: impl Fn(&Key, keyboard::Modifiers, bool) -> TableKey<Message> + 'a,
) -> Element<'a, Message> {
    Element::new(Table { content, on_key })
}

pub(crate) fn action<'a, Message: Clone + 'a>(
    content: Element<'a, Message>,
    message: Option<Message>,
    accept_enter: bool,
) -> Element<'a, Message> {
    let Some(message) = message else {
        return content;
    };
    view(content, move |key, modifiers, repeat| {
        if modifiers.is_empty()
            && (*key == Key::Named(Named::Space)
                || accept_enter && *key == Key::Named(Named::Enter))
        {
            return TableKey::Handled((!repeat).then(|| message.clone()));
        }
        TableKey::Ignored
    })
}

struct Table<'a, Message, OnKey> {
    content: Element<'a, Message>,
    on_key: OnKey,
}

#[derive(Default)]
struct Focus(bool);

impl Focusable for Focus {
    fn is_focused(&self) -> bool {
        self.0
    }

    fn focus(&mut self) {
        self.0 = true;
    }

    fn unfocus(&mut self) {
        self.0 = false;
    }
}

impl<Message, OnKey> Widget<Message, Theme, Renderer> for Table<'_, Message, OnKey>
where
    OnKey: Fn(&Key, keyboard::Modifiers, bool) -> TableKey<Message>,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Focus>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(Focus::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&[&self.content]);
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

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.focusable(None, layout.bounds(), tree.state.downcast_mut::<Focus>());
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
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
        let focus = tree.state.downcast_mut::<Focus>();
        let before = focus.0;
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                focus.0 = cursor.is_over(layout.bounds()) && cursor.is_over(*viewport);
            }
            Event::Window(window::Event::Unfocused) => focus.unfocus(),
            _ => {}
        }
        if before != focus.0 {
            shell.request_redraw();
        }
        if focus.0
            && !shell.is_event_captured()
            && let Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                modifiers,
                repeat,
                ..
            }) = event
            && let TableKey::Handled(message) = (self.on_key)(key, *modifiers, *repeat)
        {
            if let Some(message) = message {
                shell.publish(message);
            }
            shell.capture_event();
            return;
        }
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
        if tree.state.downcast_ref::<Focus>().0
            && let Some(bounds) = layout.bounds().intersection(viewport)
        {
            renderer.fill_quad(
                renderer::Quad {
                    bounds,
                    border: iced::Border::default()
                        .color(theme.palette().primary)
                        .width(1),
                    ..renderer::Quad::default()
                },
                Color::TRANSPARENT,
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
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
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
    ) -> Option<overlay::Element<'a, Message, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}
