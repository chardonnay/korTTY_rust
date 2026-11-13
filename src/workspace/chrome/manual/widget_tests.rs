//! Exercises the reader's real layout and native widget operations without an OS window.

use iced::advanced::widget::{Operation, operation};
use iced::futures::{StreamExt, executor::block_on};
use iced::{Rectangle, Renderer, Vector};
use iced_runtime::{Action, UserInterface, task, user_interface::Cache};

use super::*;

struct Position {
    page: Id,
    anchor: Id,
    viewport: Option<Rectangle>,
    target: Option<Rectangle>,
    translation: Option<Vector>,
}

impl Position {
    fn new(page: &Id, anchor: &Id) -> Self {
        Self {
            page: page.clone(),
            anchor: anchor.clone(),
            viewport: None,
            target: None,
            translation: None,
        }
    }
}

impl Operation for Position {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
        if id == Some(&self.anchor) {
            self.target = Some(bounds);
        }
    }

    fn scrollable(
        &mut self,
        id: Option<&Id>,
        bounds: Rectangle,
        _: Rectangle,
        translation: Vector,
        _: &mut dyn operation::Scrollable,
    ) {
        if id == Some(&self.page) {
            self.viewport = Some(bounds);
            self.translation = Some(translation);
        }
    }
}

#[test]
fn native_manual_navigation_reveals_section_and_source_at_zoom_extremes() {
    let section = navigation::resolve(&MANUAL, "#sftp-edit-close").unwrap();
    let page = &MANUAL[section.chapter];
    let block = page
        .blocks
        .iter()
        .position(|block| {
            block
                .text
                .starts_with("Optional idle closing is off by default.")
        })
        .unwrap();
    let excerpt = Location {
        chapter: section.chapter,
        block: Some(block),
        excerpt: Some(0.."Optional idle closing is off by default.".len()),
    };

    for scale in [MIN_MANUAL_SCALE, MAX_MANUAL_SCALE] {
        for location in [&section, &excerpt] {
            let mut workspace = Workspace::default();
            workspace.window_settings.manual_scale = scale;
            let _ = workspace.show_manual();
            let reader = workspace.chrome.manual.window.as_mut().unwrap();
            let navigation = reader.navigate(location.clone());
            let (id, scroll, anchor) = (reader.id, reader.scroll.clone(), reader.anchor.clone());
            let mut renderer = Renderer::new(Font::DEFAULT, 15.0.into());
            let mut ui = UserInterface::build(
                workspace.manual_view(id).unwrap(),
                Size::new(600.0, 375.0),
                Cache::default(),
                &mut renderer,
            );
            let mut before = Position::new(&scroll, &anchor);
            ui.operate(&renderer, &mut before);
            let viewport = before.viewport.expect("reader scrollable is mounted");
            let target = before
                .target
                .expect("the selected section or excerpt is mounted");
            assert!(
                target.y > viewport.y + viewport.height,
                "fixture starts below the viewport"
            );
            assert_eq!(before.translation.unwrap().y, 0.0);

            // Execute the actual task and chained operations as the native runtime does.
            let mut actions = task::into_stream(navigation).unwrap();
            let mut operations = 0;
            while let Some(action) = block_on(actions.next()) {
                let Action::Widget(mut operation) = action else {
                    panic!("manual navigation must only operate on native widgets");
                };
                loop {
                    operations += 1;
                    assert!(operations <= 8, "widget operation chain must terminate");
                    ui.operate(&renderer, operation.as_mut());
                    match operation.finish() {
                        operation::Outcome::Chain(next) => operation = next,
                        _ => break,
                    }
                }
            }
            assert!(operations >= 3, "reset, locate, and scroll must all run");

            let mut after = Position::new(&scroll, &anchor);
            ui.operate(&renderer, &mut after);
            let offset = after.translation.unwrap().y;
            let visible_top = after.target.unwrap().y - offset;
            let viewport = after.viewport.unwrap();
            assert!(offset > 0.0, "native scroll state must change");
            assert!(
                visible_top >= viewport.y - 1.0 && visible_top < viewport.y + viewport.height,
                "target top {visible_top} must be visible at {scale}% inside {viewport:?}"
            );
        }
    }
}
