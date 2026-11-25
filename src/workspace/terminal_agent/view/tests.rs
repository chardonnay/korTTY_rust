use super::*;
use crate::workspace::terminal_agent::flow::tests::Fixture;
use iced::advanced::widget::{Id, Operation, operation};
use iced::{Font, Rectangle, Renderer, Size, Vector};
use iced_runtime::{UserInterface, user_interface::Cache};

#[derive(Default)]
struct Bounds {
    actions: Option<Rectangle>,
    scroll: Vec<(Rectangle, Rectangle)>,
}

impl Operation for Bounds {
    fn traverse(&mut self, visit: &mut dyn FnMut(&mut dyn Operation)) {
        visit(self);
    }
    fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
        if id == Some(&Id::new("terminal-agent-actions")) {
            self.actions = Some(bounds);
        }
    }
    fn scrollable(
        &mut self,
        _: Option<&Id>,
        bounds: Rectangle,
        content: Rectangle,
        _: Vector,
        _: &mut dyn operation::Scrollable,
    ) {
        self.scroll.push((bounds, content));
    }
}

#[test]
fn agent_setup_scrolls_long_content_without_clipping_footer_in_any_mode() {
    for mode in [Mode::Run, Mode::Ask, Mode::Plan] {
        for private in [false, true] {
            let mut fixture = Fixture::with_endpoint(false, private, "127.0.0.1:12345");
            let id = fixture.open(mode);
            let run = fixture.run_mut(id);
            run.fonts.activity = 22.0;
            run.fonts.plan = 22.0;
            run.manual_directory = Some("/owned/directory".into());
            run.history.notice =
                "A long retained-history notice for the compact layout. ".repeat(12);
            for size in [
                Size::new(1100.0, 760.0),
                Size::new(800.0, 500.0),
                Size::new(640.0, 400.0),
            ] {
                let state = &fixture.workspace.terminal_agents;
                let run = &state.runs[&id];
                let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
                let mut ui = UserInterface::build(
                    run.view(None, true, false, (state, id)),
                    size,
                    Cache::default(),
                    &mut renderer,
                );
                let mut bounds = Bounds::default();
                ui.operate(&renderer, &mut bounds);
                let actions = bounds.actions.expect("the fixed footer");
                assert!(actions.height >= 28.0, "footer was compressed: {actions:?}");
                assert!(
                    actions.y >= 0.0 && actions.y + actions.height <= size.height - 10.0,
                    "footer escaped viewport: {actions:?} at {size:?}"
                );
                let [(body, content)] = bounds.scroll.as_slice() else {
                    panic!("Setup should have one scrollable content area");
                };
                assert!(body.height > 0.0 && body.y + body.height <= actions.y);
                if size.height == 400.0 {
                    assert!(content.height > body.height, "long content should scroll");
                }
            }
        }
    }
}
