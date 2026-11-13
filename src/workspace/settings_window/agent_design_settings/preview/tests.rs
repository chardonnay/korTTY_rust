use iced::advanced::{
    clipboard,
    widget::{Id, Operation},
};
use iced::widget::canvas::Program as _;
use iced::{Event, Font, Point, Rectangle, Renderer, Size, mouse};
use iced_runtime::{UserInterface, user_interface::Cache};

use super::*;

#[test]
fn changing_builtin_default_replays_only_when_the_displayed_design_changes() {
    use crate::terminal_agent::design::{Catalog, Preset};

    let mut editor = super::super::Editor::default();
    let mut catalog = Catalog::default();
    let mut preset = Preset::Standard;
    let old = editor.demo.token();
    editor.demo.update(Action::Next(old));
    editor.update(
        &mut catalog,
        &mut preset,
        Edit::Default(Preset::Classic.id().into()),
    );
    assert_eq!(preset, Preset::Classic);
    assert_eq!(editor.demo.stage, 0);
    assert_ne!(editor.demo.token().generation, old.generation);
    editor.demo.update(Action::Tick(old));
    assert_eq!(editor.demo.stage, 0);

    editor.update(
        &mut catalog,
        &mut preset,
        Edit::Select(Preset::Floating.id().into()),
    );
    editor.demo.update(Action::Next(editor.demo.token()));
    let current = editor.demo.token();
    editor.update(
        &mut catalog,
        &mut preset,
        Edit::Default(Preset::Standard.id().into()),
    );
    assert_eq!(preset, Preset::Standard);
    assert_eq!(
        editor.demo.token(),
        current,
        "an explicitly selected preview is unchanged"
    );
}

#[test]
fn demo_events_are_bound_to_the_current_replay_and_step() {
    let mut demo = Demo::default();
    let old = demo.token();
    demo.update(Action::Replay);
    demo.update(Action::Tick(old));
    assert_eq!(demo.stage, 0);
    let token = demo.token();
    demo.update(Action::Next(token));
    demo.update(Action::Next(token));
    assert_eq!(demo.stage, 1);
    while demo.stage < 3 {
        demo.update(Action::Next(demo.token()));
    }
    assert!(matches!(demo.model().prompt, Some(Prompt::Approval { .. })));
    demo.update(Action::Approve(demo.token()));
    assert_eq!(demo.stage, 4);
    demo.update(Action::Cancel(demo.token()));
    demo.update(Action::Next(demo.token()));
    assert_eq!(demo.model().state, State::Cancelled);
    assert!(demo.model().prompt.is_none());
    demo.update(Action::Replay);
    demo.update(Action::Cancel(demo.token()));
    assert_eq!(
        demo.model().steps.len(),
        1,
        "cancellation cannot invent later sample commands"
    );
}

#[test]
fn canvas_clock_publishes_only_a_scoped_demo_tick_and_stops_while_paused() {
    let demo = Demo::default();
    let owner = window::Id::unique();
    let mut preview = canvas::Preview {
        model: demo.model(),
        style: Some(inline::Style::Inline),
        token: demo.token(),
        playing: true,
        owner,
    };
    let mut clock = canvas::Clock::default();
    let now = Instant::now();
    let bounds = Rectangle::with_size(Size::new(640.0, 320.0));
    let first = preview
        .update(
            &mut clock,
            &Event::Window(window::Event::RedrawRequested(now)),
            bounds,
            mouse::Cursor::Unavailable,
        )
        .unwrap();
    assert!(first.into_inner().0.is_none());
    let due = preview
        .update(
            &mut clock,
            &Event::Window(window::Event::RedrawRequested(
                now + Duration::from_millis(1500),
            )),
            bounds,
            mouse::Cursor::Unavailable,
        )
        .unwrap();
    assert!(
        matches!(due.into_inner().0, Some(Desktop::Settings(Message::AgentDesignCatalog(id, Edit::Demo(Action::Tick(token))))) if id == owner && token == demo.token())
    );
    preview.playing = false;
    assert!(
        preview
            .update(
                &mut clock,
                &Event::Window(window::Event::RedrawRequested(
                    now + Duration::from_secs(20)
                )),
                bounds,
                mouse::Cursor::Unavailable
            )
            .is_none()
    );
    preview.playing = true;
    let resumed = preview
        .update(
            &mut clock,
            &Event::Window(window::Event::RedrawRequested(
                now + Duration::from_secs(50),
            )),
            bounds,
            mouse::Cursor::Unavailable,
        )
        .unwrap();
    assert!(
        resumed.into_inner().0.is_none(),
        "paused time must not skip a sample step"
    );
}

#[derive(Default)]
struct ReplayBounds(Option<Rectangle>);

impl Operation for ReplayBounds {
    fn traverse(&mut self, visit: &mut dyn FnMut(&mut dyn Operation)) {
        visit(self);
    }
    fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
        if id == Some(&Id::new("agent-demo-replay")) {
            self.0 = Some(bounds);
        }
    }
}

#[test]
fn native_replay_button_emits_only_an_isolated_settings_demo_event() {
    let demo = Demo::default();
    let owner = window::Id::unique();
    let layout = Layout {
        history: History::Focus,
        ..Layout::default()
    };
    let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
    let mut ui = UserInterface::build(
        view(&demo, layout, owner),
        Size::new(760.0, 700.0),
        Cache::default(),
        &mut renderer,
    );
    let mut replay = ReplayBounds::default();
    ui.operate(&renderer, &mut replay);
    let bounds = replay.0.expect("native Replay button bounds");
    let cursor = Point::new(
        bounds.x + bounds.width / 2.0,
        bounds.y + bounds.height / 2.0,
    );
    let mut messages = Vec::new();
    ui.update(
        &[
            Event::Mouse(mouse::Event::CursorMoved { position: cursor }),
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
        ],
        mouse::Cursor::Available(cursor),
        &mut renderer,
        &mut clipboard::Null,
        &mut messages,
    );
    assert!(
        matches!(messages.as_slice(), [Desktop::Settings(Message::AgentDesignCatalog(id, Edit::Demo(Action::Replay)))] if *id == owner)
    );
}
