use iced::Font;
use iced::advanced::clipboard;
use iced::widget::{button, column, container, row, text};
use iced_runtime::{UserInterface, user_interface::Cache};

use super::*;

mod animation;

#[derive(Clone, Debug, PartialEq)]
enum Message {
    Snap(Corner),
    Control,
    Terminal,
}

fn panel() -> Element<'static, Message> {
    container(column![
        row![
            container(text("Drag")).width(Length::Fill),
            button("Control").on_press(Message::Control).width(150),
        ]
        .height(32),
        text("Native history"),
    ])
    .id("pip-test-panel")
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

#[test]
fn native_drag_uses_ordered_events_and_snaps_to_each_corner() {
    let area = Size::new(800.0, 500.0);
    let panel_size = Size::new(300.0, 180.0);
    for corner in Corner::ALL {
        let origin = Corner::BottomRight.position(area, panel_size);
        let start = origin + Vector::new(15.0, 15.0);
        let finish = corner.position(area, panel_size) + Vector::new(15.0, 15.0);
        let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
        let surface = view(
            button("Terminal")
                .width(Length::Fill)
                .height(Length::Fill)
                .on_press(Message::Terminal)
                .into(),
            panel(),
            window::Id::unique(),
            Corner::BottomRight,
            panel_size,
            Options {
                draggable: true,
                reduced_motion: false,
            },
            Message::Snap,
        );
        let mut ui = UserInterface::build(surface, area, Cache::default(), &mut renderer);
        let mut messages = Vec::new();
        ui.update(
            &[
                Event::Mouse(mouse::Event::CursorMoved { position: start }),
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Event::Mouse(mouse::Event::CursorMoved { position: finish }),
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            ],
            mouse::Cursor::Available(finish),
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        assert_eq!(messages, [Message::Snap(corner)]);
    }
}

#[test]
fn native_controls_do_not_drag_or_click_through_and_lost_focus_cancels_drag() {
    let area = Size::new(800.0, 500.0);
    let panel_size = Size::new(300.0, 180.0);
    let origin = Corner::BottomRight.position(area, panel_size);
    let control = origin + Vector::new(240.0, 15.0);
    let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
    let surface = view(
        button("Terminal")
            .width(Length::Fill)
            .height(Length::Fill)
            .on_press(Message::Terminal)
            .into(),
        panel(),
        window::Id::unique(),
        Corner::BottomRight,
        panel_size,
        Options {
            draggable: true,
            reduced_motion: false,
        },
        Message::Snap,
    );
    let mut ui = UserInterface::build(surface, area, Cache::default(), &mut renderer);
    let mut messages = Vec::new();
    ui.update(
        &[
            Event::Mouse(mouse::Event::CursorMoved { position: control }),
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
        ],
        mouse::Cursor::Available(control),
        &mut renderer,
        &mut clipboard::Null,
        &mut messages,
    );
    assert_eq!(messages, [Message::Control]);
    messages.clear();
    let grip = origin + Vector::new(15.0, 15.0);
    ui.update(
        &[
            Event::Mouse(mouse::Event::CursorMoved { position: grip }),
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            Event::Window(window::Event::Unfocused),
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
        ],
        mouse::Cursor::Available(grip),
        &mut renderer,
        &mut clipboard::Null,
        &mut messages,
    );
    assert!(messages.is_empty());
}

#[test]
fn corner_geometry_fits_small_panes_and_remains_anchored_after_resize() {
    for corner in Corner::ALL {
        let small = corner.position(Size::new(100.0, 50.0), Size::new(100.0, 50.0));
        assert_eq!(small, Point::ORIGIN);
        let area = Size::new(1000.0, 600.0);
        let size = Size::new(460.0, 320.0);
        let point = corner.position(area, size);
        assert_eq!(Corner::nearest(point, area, size), corner);
        assert!(point.x >= 0.0 && point.x + size.width <= area.width);
        assert!(point.y >= 0.0 && point.y + size.height <= area.height);
    }
}
