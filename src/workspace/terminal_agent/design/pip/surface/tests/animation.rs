//! Exercise real native widget events with a counted terminal layout and deterministic time.

use std::cell::Cell;
use std::rc::Rc;

use super::*;

struct Terminal(Rc<Cell<usize>>);

impl Widget<Message, Theme, Renderer> for Terminal {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }
    fn layout(&mut self, _: &mut Tree, _: &Renderer, limits: &layout::Limits) -> layout::Node {
        self.0.set(self.0.get() + 1);
        layout::Node::new(limits.max())
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
}

struct Harness {
    element: Element<'static, Message>,
    tree: Tree,
    layout: layout::Node,
    renderer: Renderer,
    layouts: Rc<Cell<usize>>,
    run: window::Id,
    area: Size,
}

impl Harness {
    fn new(reduced: bool) -> Self {
        let layouts = Rc::new(Cell::new(0));
        let run = window::Id::unique();
        let element = Self::element(Rc::clone(&layouts), run, Corner::BottomRight, reduced);
        let tree = Tree::new(&element);
        let mut harness = Self {
            element,
            tree,
            layout: layout::Node::default(),
            renderer: Renderer::new(Font::DEFAULT, 14.0.into()),
            layouts,
            run,
            area: Size::new(800.0, 500.0),
        };
        harness.relayout(harness.area);
        harness
    }

    fn element(
        layouts: Rc<Cell<usize>>,
        run: window::Id,
        corner: Corner,
        reduced: bool,
    ) -> Element<'static, Message> {
        view(
            Element::new(Terminal(layouts)),
            panel(),
            run,
            corner,
            Size::new(300.0, 180.0),
            Options {
                draggable: true,
                reduced_motion: reduced,
            },
            Message::Snap,
        )
    }

    fn relayout(&mut self, area: Size) {
        self.area = area;
        self.layout = self.element.as_widget_mut().layout(
            &mut self.tree,
            &self.renderer,
            &layout::Limits::new(area, area),
        );
    }

    fn refresh(&mut self, run: window::Id, corner: Corner, reduced: bool) {
        self.run = run;
        self.element = Self::element(Rc::clone(&self.layouts), run, corner, reduced);
        self.element.as_widget().diff(&mut self.tree);
        self.relayout(self.area);
    }

    fn pointer(&self) -> &Pointer {
        self.tree.state.downcast_ref::<Pointer>()
    }

    fn event(&mut self, event: Event, cursor: Point) -> Vec<Message> {
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        self.element.as_widget_mut().update(
            &mut self.tree,
            &event,
            Layout::new(&self.layout),
            mouse::Cursor::Available(cursor),
            &self.renderer,
            &mut clipboard::Null,
            &mut shell,
            &Rectangle::with_size(self.area),
        );
        assert!(
            !shell.is_layout_invalid(),
            "moving PiP must not relayout the terminal"
        );
        messages
    }

    fn drag_to(&mut self, point: Point) -> Vec<Message> {
        let grip = self.pointer().node.bounds().position() + Vector::new(15.0, 15.0);
        assert!(
            self.event(
                Event::Mouse(mouse::Event::CursorMoved { position: grip }),
                grip
            )
            .is_empty()
        );
        assert!(
            self.event(
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                grip
            )
            .is_empty()
        );
        let end = point + Vector::new(15.0, 15.0);
        assert!(
            self.event(
                Event::Mouse(mouse::Event::CursorMoved { position: end }),
                end
            )
            .is_empty()
        );
        self.event(
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            end,
        )
    }

    fn redraw(&mut self, now: Instant) {
        assert!(
            self.event(
                Event::Window(window::Event::RedrawRequested(now)),
                Point::ORIGIN
            )
            .is_empty()
        );
    }
}

#[test]
fn native_snap_uses_reference_spring_without_delaying_selection_or_relayout() {
    let mut harness = Harness::new(false);
    let from = Point::new(160.0, 90.0);
    assert_eq!(harness.drag_to(from), [Message::Snap(Corner::TopLeft)]);
    assert_eq!(harness.pointer().corner, Corner::TopLeft);
    assert_eq!(harness.pointer().node.bounds().position(), from);
    assert_eq!(harness.layouts.get(), 1);
    // Process the authoritative model change, as the normal Iced tree diff does.
    harness.refresh(harness.run, Corner::TopLeft, false);
    let layouts = harness.layouts.get();
    let now = Instant::now();
    harness.redraw(now);
    assert!(harness.pointer().node.bounds().position().distance(from) < 0.001);
    harness.redraw(now + Duration::from_millis(210));
    let halfway = harness.pointer().node.bounds();
    assert!(
        halfway.y < 12.0 && halfway.y >= 0.0,
        "spring overshoots and stays within the pane"
    );
    assert!(halfway.x >= 0.0 && halfway.x + halfway.width <= harness.area.width);
    harness.redraw(now + SNAP_DURATION);
    assert_eq!(
        harness.pointer().node.bounds().position(),
        Point::new(12.0, 12.0)
    );
    assert!(harness.pointer().snap.is_none());
    assert_eq!(harness.layouts.get(), layouts);
}

#[test]
fn native_snap_interrupts_on_drag_focus_reduced_motion_resize_and_run_change() {
    let now = Instant::now();
    let from = Point::new(160.0, 90.0);
    let mut harness = Harness::new(false);
    harness.drag_to(from);
    harness.redraw(now);
    harness.redraw(now + Duration::from_millis(50));
    let grip = harness.pointer().node.bounds().position() + Vector::new(15.0, 15.0);
    harness.event(
        Event::Mouse(mouse::Event::CursorMoved { position: grip }),
        grip,
    );
    harness.event(
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
        grip,
    );
    assert!(harness.pointer().snap.is_none());
    assert!(harness.pointer().offset.is_some());
    let held = harness.pointer().node.bounds().position();
    harness.redraw(now + Duration::from_secs(1));
    assert_eq!(harness.pointer().node.bounds().position(), held);
    harness.event(Event::Window(window::Event::Unfocused), grip);
    assert!(harness.pointer().offset.is_none());
    assert_eq!(
        harness.pointer().node.bounds().position(),
        Point::new(12.0, 12.0)
    );

    harness.drag_to(from);
    harness.redraw(now);
    harness.event(Event::Window(window::Event::Unfocused), Point::ORIGIN);
    assert!(harness.pointer().snap.is_none());
    assert_eq!(
        harness.pointer().node.bounds().position(),
        Point::new(12.0, 12.0)
    );

    harness.drag_to(from);
    harness.refresh(harness.run, Corner::TopLeft, true);
    assert!(harness.pointer().snap.is_none());
    assert_eq!(
        harness.pointer().node.bounds().position(),
        Point::new(12.0, 12.0)
    );
    assert_eq!(harness.drag_to(from), [Message::Snap(Corner::TopLeft)]);
    assert!(
        harness.pointer().snap.is_none(),
        "reduced motion settles immediately"
    );
    harness.refresh(harness.run, Corner::TopLeft, false);
    harness.drag_to(from);
    harness.relayout(Size::new(100.0, 50.0));
    assert!(harness.pointer().snap.is_none());
    assert_eq!(
        harness.pointer().node.bounds(),
        Rectangle::with_size(harness.area)
    );

    harness.relayout(Size::new(800.0, 500.0));
    harness.drag_to(from);
    harness.refresh(window::Id::unique(), Corner::BottomRight, false);
    assert!(harness.pointer().snap.is_none());
    assert_eq!(
        harness.pointer().node.bounds().position(),
        Corner::BottomRight.position(harness.area, Size::new(300.0, 180.0))
    );
}

#[test]
fn moving_native_controls_use_the_visible_position_without_click_through() {
    let mut harness = Harness::new(false);
    harness.drag_to(Point::new(160.0, 90.0));
    let now = Instant::now();
    harness.redraw(now);
    harness.redraw(now + Duration::from_millis(70));
    let control = harness.pointer().node.bounds().position() + Vector::new(240.0, 15.0);
    harness.event(
        Event::Mouse(mouse::Event::CursorMoved { position: control }),
        control,
    );
    assert!(
        harness
            .event(
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                control
            )
            .is_empty()
    );
    assert_eq!(
        harness.event(
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            control
        ),
        [Message::Control]
    );
    assert!(
        harness.pointer().snap.is_some(),
        "control clicks do not restart dragging"
    );
}
