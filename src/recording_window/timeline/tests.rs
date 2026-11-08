use iced::advanced::clipboard;
use iced::{Font, Pixels, Point};

use super::*;

#[test]
fn recording_scrub_widget_starts_on_press_and_releases_outside_its_bounds() {
    let renderer = Renderer::new(Font::DEFAULT, Pixels(13.0));
    let limits = layout::Limits::new(Size::ZERO, Size::new(200.0, 40.0));
    let mut widget = view(Duration::from_secs(10), Duration::from_secs(20));
    let mut tree = Tree::new(widget.as_widget());
    let node = widget.as_widget_mut().layout(&mut tree, &renderer, &limits);
    let bounds = node.bounds();
    let midpoint = Point::new(bounds.center_x(), bounds.center_y());
    let mut dispatch = |event, cursor| {
        let mut messages = Vec::new();
        widget.as_widget_mut().update(
            &mut tree,
            &event,
            Layout::new(&node),
            cursor,
            &renderer,
            &mut clipboard::Null,
            &mut Shell::new(&mut messages),
            &bounds,
        );
        messages
    };
    let press = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
    let release = Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left));
    assert!(matches!(
        dispatch(press.clone(), mouse::Cursor::Available(midpoint)).as_slice(),
        [Message::ScrubStarted]
    ));
    assert!(matches!(
        dispatch(release.clone(), mouse::Cursor::Unavailable).as_slice(),
        [Message::ScrubFinished]
    ));
    assert!(dispatch(release.clone(), mouse::Cursor::Unavailable).is_empty());
    assert!(matches!(
        dispatch(press, mouse::Cursor::Available(midpoint)).as_slice(),
        [Message::ScrubStarted]
    ));
    assert!(matches!(
        dispatch(
            Event::Window(window::Event::Unfocused),
            mouse::Cursor::Unavailable
        )
        .as_slice(),
        [Message::ScrubFinished]
    ));
    assert!(dispatch(release, mouse::Cursor::Unavailable).is_empty());

    let owner = touch::Finger(7);
    let other = touch::Finger(8);
    let later = Point::new(bounds.x + bounds.width * 0.75, midpoint.y);
    assert!(matches!(
        dispatch(
            Event::Touch(touch::Event::FingerPressed {
                id: owner,
                position: midpoint
            }),
            mouse::Cursor::Available(midpoint),
        )
        .as_slice(),
        [Message::ScrubStarted]
    ));
    for event in [
        Event::Touch(touch::Event::FingerPressed {
            id: other,
            position: later,
        }),
        Event::Touch(touch::Event::FingerMoved {
            id: other,
            position: later,
        }),
        Event::Touch(touch::Event::FingerLifted {
            id: other,
            position: later,
        }),
        Event::Touch(touch::Event::FingerLost {
            id: other,
            position: later,
        }),
        Event::Mouse(mouse::Event::CursorMoved { position: later }),
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
    ] {
        assert!(dispatch(event, mouse::Cursor::Available(later)).is_empty());
    }
    assert!(matches!(
        dispatch(
            Event::Touch(touch::Event::FingerMoved { id: owner, position: later }),
            mouse::Cursor::Available(later),
        ).as_slice(),
        [Message::Scrub(seconds)] if (*seconds - 15.0).abs() < 0.001
    ));
    assert!(matches!(
        dispatch(
            Event::Touch(touch::Event::FingerLost {
                id: owner,
                position: later
            }),
            mouse::Cursor::Unavailable,
        )
        .as_slice(),
        [Message::ScrubFinished]
    ));
}

#[test]
fn recording_scrub_arrow_step_uses_reference_range_and_clamps_endpoints() {
    use keyboard::{Key, key::Named};

    let forward = Key::Named(Named::ArrowRight);
    let backward = Key::Named(Named::ArrowLeft);
    assert_eq!(arrow_position(&forward, 2.0, 10.0), Some(3.0));
    assert_eq!(arrow_position(&forward, 2.0, 300.0), Some(17.0));
    assert_eq!(arrow_position(&forward, 2.0, 1200.0), Some(32.0));
    assert_eq!(arrow_position(&backward, 2.0, 1200.0), Some(0.0));
    assert_eq!(arrow_position(&forward, 1199.0, 1200.0), Some(1200.0));
}
