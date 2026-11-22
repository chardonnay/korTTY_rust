use iced::advanced::{clipboard, graphics::Viewport};
use iced::widget::{button, opaque, row, space, stack, text};
use iced::{Color, Font};
use iced_runtime::{UserInterface, user_interface::Cache};

use super::*;

#[test]
fn bloom_reverses_from_the_current_radius_and_respects_reduced_motion() {
    let now = Instant::now();
    let mut motion = Motion::default();
    assert!(motion.redraw(true, false, now));
    assert_eq!(motion.progress, 0.0);
    let halfway = now + OPEN / 2;
    assert!(motion.redraw(true, false, halfway));
    let partial = motion.progress;
    assert!(partial > 0.5 && partial < 1.0);
    assert!(motion.redraw(false, false, halfway));
    assert_eq!(motion.progress, partial);
    let folding = halfway + CLOSE / 2;
    assert!(motion.redraw(false, false, folding));
    let smaller = motion.progress;
    assert!(smaller > 0.0 && smaller < partial);
    assert!(motion.redraw(true, false, folding));
    assert_eq!(motion.progress, smaller);
    assert!(!motion.redraw(true, false, folding + OPEN));
    assert_eq!(motion.progress, 1.0);
    assert!(motion.redraw(false, false, folding + OPEN + CLOSE));
    assert!(!motion.redraw(false, false, folding + OPEN + CLOSE * 2));
    assert_eq!(motion.progress, 0.0);
    assert!(!motion.redraw(true, true, now));
    assert_eq!(motion.progress, 1.0);
    assert!(!motion.redraw(false, true, now));
    assert_eq!(motion.progress, 0.0);
}

#[test]
fn circular_pixels_and_pointer_hits_follow_the_same_reveal() {
    let scene_size = Size::new(380.0, 300.0);
    let physical = Size::new(760, 600);
    let far = Point::new(178.0, 125.0);
    let near = Point::new(314.0, 204.0);
    let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
    let mut pixels = tiny_skia::Pixmap::new(physical.width, physical.height).unwrap();
    let mut mask = tiny_skia::Mask::new(physical.width, physical.height).unwrap();
    let scene = |open| {
        let step = || {
            button(text("Step"))
                .width(100)
                .height(100)
                .style(|_, _| button::Style::default().with_background(Color::WHITE))
                .on_press(AgentMessage::Focus)
        };
        let bloom = Element::new(Bloom {
            steps: opaque(row![step(), step()]),
            footer: space().into(),
            key: (Some(1), None),
            open,
            reduced_motion: false,
        });
        stack![
            button(space())
                .width(Length::Fill)
                .height(Length::Fill)
                .style(|_, _| button::Style::default().with_background(Color::BLACK))
                .on_press(AgentMessage::Cancel),
            super::super::super::super::fade::view(bloom, 0.5),
        ]
    };
    let mut render = |ui: &mut UserInterface<'_, AgentMessage, Theme, Renderer>,
                      renderer: &mut Renderer,
                      now| {
        let cursor = mouse::Cursor::Unavailable;
        let _ = ui.update(
            &[Event::Window(window::Event::RedrawRequested(now))],
            cursor,
            renderer,
            &mut clipboard::Null,
            &mut Vec::new(),
        );
        ui.draw(renderer, &Theme::Dark, &renderer::Style::default(), cursor);
        renderer.draw(
            &mut pixels.as_mut(),
            &mut mask,
            &Viewport::with_physical_size(physical, 2.0),
            &[Rectangle::with_size(scene_size)],
            Color::BLACK,
        );
        let sample = |point: Point| {
            pixels
                .pixel((point.x * 2.0) as u32, (point.y * 2.0) as u32)
                .unwrap()
                .red()
        };
        (sample(far), sample(near))
    };
    let click = |ui: &mut UserInterface<'_, AgentMessage, Theme, Renderer>,
                 renderer: &mut Renderer,
                 point| {
        let mut actions = Vec::new();
        let _ = ui.update(
            &[
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            ],
            mouse::Cursor::Available(point),
            renderer,
            &mut clipboard::Null,
            &mut actions,
        );
        actions
    };
    let start = Instant::now();
    let mut ui = UserInterface::build(scene(true), scene_size, Cache::default(), &mut renderer);
    assert_eq!(render(&mut ui, &mut renderer, start), (0, 0));
    let (far_color, near_color) = render(&mut ui, &mut renderer, start + Duration::from_millis(36));
    assert_eq!(far_color, 0);
    assert!((120..=135).contains(&near_color), "{near_color}");
    assert!(matches!(
        click(&mut ui, &mut renderer, far).as_slice(),
        [AgentMessage::Cancel]
    ));
    assert!(matches!(
        click(&mut ui, &mut renderer, near).as_slice(),
        [AgentMessage::Focus]
    ));
    let full = render(&mut ui, &mut renderer, start + OPEN);
    assert_eq!(full, (near_color, near_color));

    let mut ui = UserInterface::build(scene(false), scene_size, ui.into_cache(), &mut renderer);
    let close = start + OPEN + FRAME;
    assert_eq!(render(&mut ui, &mut renderer, close), full);
    assert!(click(&mut ui, &mut renderer, near).is_empty());
    assert_eq!(render(&mut ui, &mut renderer, close + CLOSE), (0, 0));
    assert!(matches!(
        click(&mut ui, &mut renderer, near).as_slice(),
        [AgentMessage::Cancel]
    ));
}
