use std::{cell::Cell, rc::Rc};

use iced::advanced::{Layout, Shell, Widget, clipboard, layout, renderer, widget::Tree};
use iced::keyboard::{self, Key, Location, Modifiers, key};
use iced::widget::{Id, Space, column, pick_list, tooltip};
use iced::{Element, Event, Font, Length, Point, Rectangle, Renderer, Size, Theme, Vector, mouse};

struct Hint(Rc<Cell<usize>>);

impl Widget<bool, Theme, Renderer> for Hint {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Shrink, Length::Shrink)
    }

    fn layout(&mut self, _: &mut Tree, _: &Renderer, _: &layout::Limits) -> layout::Node {
        layout::Node::new(Size::new(180.0, 20.0))
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
        self.0.set(self.0.get() + 1);
    }
}

#[test]
fn combination_hint_can_yield_to_upward_and_downward_menus_and_return_after_escape() {
    for y in [20.0, 260.0] {
        for hide in [false, true] {
            let draws = Rc::new(Cell::new(0));
            let hint: Element<'_, bool> = Element::new(Hint(draws.clone()));
            let picker = pick_list(
                ["Standard", "Classic", "Floating"],
                Some("Standard"),
                |_| true,
            )
            .keyboard_navigation(Id::unique());
            let tooltip = tooltip(picker, hint, tooltip::Position::Top);
            let tooltip = if hide {
                tooltip.hide_on_content_overlay(true)
            } else {
                tooltip
            };
            let mut view: Element<'_, bool> = column![Space::new().height(y), tooltip].into();
            let size = Size::new(320.0, 360.0);
            let viewport = Rectangle::with_size(size);
            let cursor = mouse::Cursor::Available(Point::new(10.0, y + 10.0));
            let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
            let mut tree = Tree::new(view.as_widget());
            let node = view.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, size),
            );
            let mut messages = Vec::new();
            for (event, expected) in [
                (
                    Event::Mouse(mouse::Event::CursorMoved {
                        position: Point::new(10.0, y + 10.0),
                    }),
                    1,
                ),
                (
                    Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                    usize::from(!hide),
                ),
            ] {
                view.as_widget_mut().update(
                    &mut tree,
                    &event,
                    Layout::new(&node),
                    cursor,
                    &renderer,
                    &mut clipboard::Null,
                    &mut Shell::new(&mut messages),
                    &viewport,
                );
                draws.set(0);
                let mut overlay = view
                    .as_widget_mut()
                    .overlay(
                        &mut tree,
                        Layout::new(&node),
                        &renderer,
                        &viewport,
                        Vector::ZERO,
                    )
                    .expect("hover hint or open menu");
                let overlay = overlay.as_overlay_mut();
                let overlay_node = overlay.layout(&renderer, size);
                overlay.draw(
                    &mut renderer,
                    &Theme::Dark,
                    &renderer::Style::default(),
                    Layout::new(&overlay_node),
                    cursor,
                );
                assert_eq!(draws.get(), expected, "y={y}, hide={hide}");
            }
            {
                let mut overlay = view
                    .as_widget_mut()
                    .overlay(
                        &mut tree,
                        Layout::new(&node),
                        &renderer,
                        &viewport,
                        Vector::ZERO,
                    )
                    .unwrap();
                let overlay = overlay.as_overlay_mut();
                let overlay_node = overlay.layout(&renderer, size);
                let escape = Event::Keyboard(keyboard::Event::KeyPressed {
                    key: Key::Named(key::Named::Escape),
                    modified_key: Key::Named(key::Named::Escape),
                    physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
                    location: Location::Standard,
                    modifiers: Modifiers::empty(),
                    text: None,
                    repeat: false,
                });
                let mut shell = Shell::new(&mut messages);
                overlay.update(
                    &escape,
                    Layout::new(&overlay_node),
                    cursor,
                    &renderer,
                    &mut clipboard::Null,
                    &mut shell,
                );
                assert!(shell.is_event_captured());
            }
            draws.set(0);
            let mut overlay = view
                .as_widget_mut()
                .overlay(
                    &mut tree,
                    Layout::new(&node),
                    &renderer,
                    &viewport,
                    Vector::ZERO,
                )
                .expect("hint returns after menu closes");
            let overlay = overlay.as_overlay_mut();
            let overlay_node = overlay.layout(&renderer, size);
            overlay.draw(
                &mut renderer,
                &Theme::Dark,
                &renderer::Style::default(),
                Layout::new(&overlay_node),
                cursor,
            );
            assert_eq!(draws.get(), 1);
            assert!(messages.is_empty());
        }
    }
}
