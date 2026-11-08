use std::cell::RefCell;
use std::sync::Arc;

use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{
    Clipboard, Layout, Overlay, Shell, Widget, clipboard, layout, mouse, overlay, renderer,
};
use iced::{Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector};

/// The clipboard used by all managed internal windows.
#[derive(Default)]
pub struct InternalClipboard {
    contents: RefCell<Option<Contents>>,
}

enum Contents {
    Text(String),
    Png(Arc<[u8]>),
}

impl InternalClipboard {
    pub fn get(&self) -> Option<String> {
        match self.contents.borrow().as_ref()? {
            Contents::Text(text) => Some(text.clone()),
            Contents::Png(_) => None,
        }
    }

    pub fn set(&self, text: String) {
        *self.contents.borrow_mut() = Some(Contents::Text(text));
    }

    pub fn png(&self) -> Option<Arc<[u8]>> {
        match self.contents.borrow().as_ref()? {
            Contents::Png(png) => Some(Arc::clone(png)),
            Contents::Text(_) => None,
        }
    }

    pub fn set_png(&self, png: Arc<[u8]>) {
        *self.contents.borrow_mut() = Some(Contents::Png(png));
    }

    pub fn isolate<'a, Message: 'a>(
        &'a self,
        element: Element<'a, Message>,
    ) -> Element<'a, Message> {
        Element::new(IsolatedWidget {
            element,
            clipboard: self,
        })
    }
}

impl Clipboard for &InternalClipboard {
    fn read(&self, _kind: clipboard::Kind) -> Option<String> {
        self.get()
    }

    fn write(&mut self, _kind: clipboard::Kind, contents: String) {
        self.set(contents);
    }
}

struct IsolatedWidget<'a, Message> {
    element: Element<'a, Message>,
    clipboard: &'a InternalClipboard,
}

impl<'a, Message: 'a> Widget<Message, Theme, Renderer> for IsolatedWidget<'a, Message> {
    fn tag(&self) -> tree::Tag {
        self.element.as_widget().tag()
    }

    fn state(&self) -> tree::State {
        self.element.as_widget().state()
    }

    fn children(&self) -> Vec<Tree> {
        self.element.as_widget().children()
    }

    fn diff(&self, tree: &mut Tree) {
        self.element.as_widget().diff(tree);
    }

    fn size(&self) -> Size<Length> {
        self.element.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.element.as_widget().size_hint()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.element.as_widget_mut().layout(tree, renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.element
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
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.element.as_widget_mut().update(
            tree,
            event,
            layout,
            cursor,
            renderer,
            &mut self.clipboard,
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
        self.element
            .as_widget()
            .draw(tree, renderer, theme, style, layout, cursor, viewport);
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.element
            .as_widget()
            .mouse_interaction(tree, layout, cursor, viewport, renderer)
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<OverlayElement<'b, Message>> {
        self.element
            .as_widget_mut()
            .overlay(tree, layout, renderer, viewport, translation)
            .map(|element| isolate_overlay(element, self.clipboard))
    }
}

type OverlayElement<'a, Message> = overlay::Element<'a, Message, Theme, Renderer>;

fn isolate_overlay<'a, Message: 'a>(
    element: OverlayElement<'a, Message>,
    clipboard: &'a InternalClipboard,
) -> OverlayElement<'a, Message> {
    overlay::Element::new(Box::new(IsolatedOverlay { element, clipboard }))
}

struct IsolatedOverlay<'a, Message> {
    element: OverlayElement<'a, Message>,
    clipboard: &'a InternalClipboard,
}

impl<'a, Message: 'a> Overlay<Message, Theme, Renderer> for IsolatedOverlay<'a, Message> {
    fn index(&self) -> f32 {
        self.element.as_overlay().index()
    }

    fn layout(&mut self, renderer: &Renderer, bounds: Size) -> layout::Node {
        self.element.as_overlay_mut().layout(renderer, bounds)
    }

    fn draw(
        &self,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        self.element
            .as_overlay()
            .draw(renderer, theme, style, layout, cursor);
    }

    fn operate(&mut self, layout: Layout<'_>, renderer: &Renderer, operation: &mut dyn Operation) {
        self.element
            .as_overlay_mut()
            .operate(layout, renderer, operation);
    }

    fn update(
        &mut self,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
    ) {
        self.element.as_overlay_mut().update(
            event,
            layout,
            cursor,
            renderer,
            &mut self.clipboard,
            shell,
        );
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.element
            .as_overlay()
            .mouse_interaction(layout, cursor, renderer)
    }

    fn overlay<'b>(
        &'b mut self,
        layout: Layout<'b>,
        renderer: &Renderer,
    ) -> Option<OverlayElement<'b, Message>> {
        self.element
            .as_overlay_mut()
            .overlay(layout, renderer)
            .map(|element| isolate_overlay(element, self.clipboard))
    }
}

#[cfg(test)]
mod tests {
    use iced::advanced::widget::{Id, operation::focusable};
    use iced::keyboard::{self, Key, Location, Modifiers, key};
    use iced::widget::text_input;
    use iced::{Font, Pixels};

    use super::*;

    // Test double for the system clipboard. Every access is a policy failure.
    struct SystemClipboardProbe;

    impl Clipboard for SystemClipboardProbe {
        fn read(&self, _: clipboard::Kind) -> Option<String> {
            panic!("internal input read the system clipboard")
        }

        fn write(&mut self, _: clipboard::Kind, _: String) {
            panic!("internal input wrote the system clipboard")
        }
    }

    fn key_press(character: &'static str, physical: key::Code) -> Event {
        Event::Keyboard(keyboard::Event::KeyPressed {
            key: Key::Character(character.into()),
            modified_key: Key::Character(character.into()),
            physical_key: key::Physical::Code(physical),
            location: Location::Standard,
            modifiers: Modifiers::COMMAND,
            text: None,
            repeat: false,
        })
    }

    #[test]
    fn native_text_input_copies_and_pastes_internally_with_existing_focus() {
        let clipboard = InternalClipboard::default();
        assert_eq!(clipboard.get(), None);
        let renderer = Renderer::new(Font::DEFAULT, Pixels(16.0));
        let id = Id::unique();
        let mut field: Element<'_, String> = text_input("", "existing text")
            .id(id.clone())
            .on_input(|text| text)
            .into();
        let mut tree = Tree::new(field.as_widget());
        let limits = layout::Limits::new(Size::ZERO, Size::new(220.0, 50.0));
        let node = field.as_widget_mut().layout(&mut tree, &renderer, &limits);
        field.as_widget_mut().operate(
            &mut tree,
            Layout::new(&node),
            &renderer,
            &mut focusable::focus(id),
        );

        let mut field = clipboard.isolate(field);
        tree.diff(field.as_widget());
        let node = field.as_widget_mut().layout(&mut tree, &renderer, &limits);
        let viewport = node.bounds();
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        let mut system = SystemClipboardProbe;

        for event in [
            Event::Keyboard(keyboard::Event::ModifiersChanged(Modifiers::COMMAND)),
            key_press("a", key::Code::KeyA),
            key_press("c", key::Code::KeyC),
        ] {
            field.as_widget_mut().update(
                &mut tree,
                &event,
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &renderer,
                &mut system,
                &mut shell,
                &viewport,
            );
        }
        assert_eq!(clipboard.get().as_deref(), Some("existing text"));

        clipboard.set("internal paste".into());
        field.as_widget_mut().update(
            &mut tree,
            &key_press("v", key::Code::KeyV),
            Layout::new(&node),
            mouse::Cursor::Unavailable,
            &renderer,
            &mut system,
            &mut shell,
            &viewport,
        );
        assert_eq!(messages, ["internal paste"]);
    }

    // Test doubles for a widget's overlay and its nested child.
    struct OverlayWidgetProbe;

    impl Widget<String, Theme, Renderer> for OverlayWidgetProbe {
        fn size(&self) -> Size<Length> {
            Size::new(Length::Fixed(20.0), Length::Fixed(10.0))
        }

        fn layout(&mut self, _: &mut Tree, _: &Renderer, _: &layout::Limits) -> layout::Node {
            layout::Node::new(Size::new(20.0, 10.0))
        }

        fn draw(
            &self,
            _: &Tree,
            _: &mut Renderer,
            _: &Theme,
            _: &renderer::Style,
            _: Layout<'_>,
            _: mouse::Cursor,
            _: &Rectangle,
        ) {
        }

        fn overlay<'a>(
            &'a mut self,
            _: &'a mut Tree,
            _: Layout<'a>,
            _: &Renderer,
            _: &Rectangle,
            _: Vector,
        ) -> Option<OverlayElement<'a, String>> {
            Some(overlay::Element::new(Box::new(OverlayProbe(false))))
        }
    }

    struct OverlayProbe(bool);

    impl Overlay<String, Theme, Renderer> for OverlayProbe {
        fn index(&self) -> f32 {
            if self.0 { 4.0 } else { 3.0 }
        }

        fn layout(&mut self, _: &Renderer, bounds: Size) -> layout::Node {
            layout::Node::new(bounds)
        }

        fn draw(
            &self,
            _: &mut Renderer,
            _: &Theme,
            _: &renderer::Style,
            _: Layout<'_>,
            _: mouse::Cursor,
        ) {
        }

        fn update(
            &mut self,
            _: &Event,
            _: Layout<'_>,
            _: mouse::Cursor,
            _: &Renderer,
            clipboard: &mut dyn Clipboard,
            shell: &mut Shell<'_, String>,
        ) {
            if self.0 {
                shell.publish(clipboard.read(clipboard::Kind::Standard).unwrap());
                clipboard.write(clipboard::Kind::Standard, "nested write".into());
            } else {
                shell.publish(clipboard.read(clipboard::Kind::Primary).unwrap());
                clipboard.write(clipboard::Kind::Primary, "outer write".into());
            }
        }

        fn overlay<'a>(
            &'a mut self,
            _: Layout<'a>,
            _: &Renderer,
        ) -> Option<OverlayElement<'a, String>> {
            (!self.0).then(|| overlay::Element::new(Box::new(OverlayProbe(true))))
        }
    }

    #[test]
    fn overlays_and_nested_overlays_share_the_internal_primary_and_standard_buffer() {
        let clipboard = InternalClipboard::default();
        clipboard.set("internal seed".into());
        let renderer = Renderer::new(Font::DEFAULT, Pixels(16.0));
        let mut widget = clipboard.isolate(Element::new(OverlayWidgetProbe));
        let mut tree = Tree::new(widget.as_widget());
        let limits = layout::Limits::new(Size::ZERO, Size::new(220.0, 50.0));
        let node = widget.as_widget_mut().layout(&mut tree, &renderer, &limits);
        let viewport = node.bounds();
        let mut outer = widget
            .as_widget_mut()
            .overlay(
                &mut tree,
                Layout::new(&node),
                &renderer,
                &viewport,
                Vector::ZERO,
            )
            .unwrap();
        assert_eq!(outer.as_overlay().index(), 3.0);
        let mut system = SystemClipboardProbe;
        let event = Event::Mouse(mouse::Event::CursorEntered);
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        let outer_node = outer.as_overlay_mut().layout(&renderer, viewport.size());
        outer.as_overlay_mut().update(
            &event,
            Layout::new(&outer_node),
            mouse::Cursor::Unavailable,
            &renderer,
            &mut system,
            &mut shell,
        );
        let mut nested = outer
            .as_overlay_mut()
            .overlay(Layout::new(&outer_node), &renderer)
            .unwrap();
        assert_eq!(nested.as_overlay().index(), 4.0);
        let nested_node = nested.as_overlay_mut().layout(&renderer, viewport.size());
        nested.as_overlay_mut().update(
            &event,
            Layout::new(&nested_node),
            mouse::Cursor::Unavailable,
            &renderer,
            &mut system,
            &mut shell,
        );
        assert_eq!(messages, ["internal seed", "outer write"]);
        assert_eq!(clipboard.get().as_deref(), Some("nested write"));
    }
}
