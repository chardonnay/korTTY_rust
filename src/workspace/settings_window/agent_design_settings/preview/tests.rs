use iced::advanced::{
    clipboard,
    widget::{Id, Operation},
};
use iced::widget::canvas::Program as _;
use iced::{Event, Font, Point, Rectangle, Renderer, Size, mouse};
use iced_runtime::{UserInterface, user_interface::Cache};

use super::*;

#[test]
fn previewing_one_module_leaves_the_saved_roles_and_default_intact() {
    use super::super::Module;
    use crate::terminal_agent::design::{Catalog, Preset, Status};

    let mut editor = super::super::Editor::default();
    let mut catalog = Catalog::default();
    let mut preset = Preset::Standard;
    let saved = catalog.layout(preset.id()).unwrap();
    editor.update(
        &mut catalog,
        &mut preset,
        Edit::Preview(Some(Module::Status(Status::Minimap))),
    );
    assert_eq!(
        editor.preview_layout(&catalog, preset),
        Layout {
            status: Status::Minimap,
            ..saved
        }
    );
    assert_eq!(catalog.layout(preset.id()).unwrap(), saved);
    assert_eq!(catalog.default_id(preset), Preset::Standard.id());
    editor.update(&mut catalog, &mut preset, Edit::Preview(None));
    assert_eq!(editor.preview_layout(&catalog, preset), saved);
}

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
        reduced_motion: false,
        cursor_design: false,
        cursor_review: false,
        status_frame: None,
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

#[test]
fn reduced_motion_skips_intermediate_frames_and_keeps_demo_timing() {
    let demo = Demo::default();
    let mut preview = canvas::Preview {
        model: demo.model(),
        style: Some(inline::Style::Inline),
        token: demo.token(),
        playing: true,
        owner: window::Id::unique(),
        reduced_motion: true,
        cursor_design: false,
        cursor_review: false,
        status_frame: None,
    };
    let bounds = Rectangle::with_size(Size::new(640.0, 320.0));
    let mut clock = canvas::Clock::default();
    let now = Instant::now();
    let redraw = |preview: &canvas::Preview, clock: &mut canvas::Clock, now| {
        preview
            .update(
                clock,
                &Event::Window(window::Event::RedrawRequested(now)),
                bounds,
                mouse::Cursor::Unavailable,
            )
            .unwrap()
            .into_inner()
    };
    let next = now + Duration::from_millis(1500);
    assert_eq!(
        redraw(&preview, &mut clock, now).1,
        window::RedrawRequest::At(next)
    );
    assert!(matches!(
        redraw(&preview, &mut clock, next).0,
        Some(Desktop::Settings(Message::AgentDesignCatalog(
            _,
            Edit::Demo(Action::Tick(_))
        )))
    ));

    preview.playing = false;
    preview.status_frame = Some(next);
    assert_eq!(
        redraw(&preview, &mut clock, now).1,
        window::RedrawRequest::At(next)
    );
    assert!(matches!(
        redraw(&preview, &mut clock, next).0,
        Some(Desktop::Settings(Message::AgentDesignCatalog(
            _,
            Edit::Demo(Action::StatusDeadline)
        )))
    ));
}

#[derive(Default)]
struct ReplayBounds(Option<Rectangle>);

#[test]
fn cursor_preview_fades_after_cancellation_and_reduced_motion_stops_the_frames() {
    let mut demo = Demo::default();
    for _ in 0..3 {
        demo.update(Action::Next(demo.token()));
    }
    let mut preview = canvas::Preview {
        model: demo.model(),
        style: None,
        token: demo.token(),
        playing: true,
        owner: window::Id::unique(),
        reduced_motion: false,
        cursor_design: true,
        cursor_review: true,
        status_frame: None,
    };
    let bounds = Rectangle::with_size(Size::new(640.0, 320.0));
    let now = Instant::now();
    let redraw = |preview: &canvas::Preview, clock: &mut canvas::Clock, at| {
        preview.update(
            clock,
            &Event::Window(window::Event::RedrawRequested(at)),
            bounds,
            mouse::Cursor::Unavailable,
        )
    };
    let mut clock = canvas::Clock::default();
    redraw(&preview, &mut clock, now).unwrap();
    preview.cursor_review = false;
    preview.playing = false;
    let closed = now + Duration::from_millis(20);
    let (_, request, _) = redraw(&preview, &mut clock, closed).unwrap().into_inner();
    assert_eq!(
        request,
        window::RedrawRequest::At(closed + Duration::from_millis(16))
    );
    let tail = closed + Duration::from_millis(175);
    let (_, request, _) = redraw(&preview, &mut clock, tail).unwrap().into_inner();
    assert_eq!(
        request,
        window::RedrawRequest::At(closed + Duration::from_millis(180))
    );
    assert!(redraw(&preview, &mut clock, closed + Duration::from_millis(180)).is_none());

    preview.playing = true;
    preview.cursor_review = true;
    let next = now + Duration::from_secs(1);
    redraw(&preview, &mut clock, next).unwrap();
    preview.playing = false;
    preview.cursor_review = false;
    preview.reduced_motion = true;
    assert!(redraw(&preview, &mut clock, next + Duration::from_millis(20)).is_none());
}

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
        history: super::super::History::Focus,
        ..Layout::default()
    };
    let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
    let mut ui = UserInterface::build(
        view(
            &demo,
            layout,
            super::super::DockPreferences::default(),
            6,
            false,
            owner,
        ),
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

#[test]
fn tab_reveals_preview_and_navigation_opens_only_the_current_sample() {
    use iced::Vector;
    use iced::advanced::widget::operation;
    use iced::keyboard::{Key, key::Named};

    use super::super::super::{Section, Workspace};
    use crate::settings_search::test_input;
    use crate::terminal_agent::design::History;

    struct PreviewMeasure {
        page: Id,
        offset: f32,
        visible_bottom: f32,
        answer_bottom: Option<f32>,
        has_details: bool,
    }

    impl Operation for PreviewMeasure {
        fn traverse(&mut self, visit: &mut dyn FnMut(&mut dyn Operation)) {
            visit(self);
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
                self.offset = translation.y;
                self.visible_bottom = bounds.y + bounds.height + translation.y;
            }
        }

        fn text(&mut self, _: Option<&Id>, bounds: Rectangle, value: &str) {
            if value == ANSWER {
                self.answer_bottom = Some(bounds.y + bounds.height);
            }
            self.has_details |= value == "Close details";
        }
    }

    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    let _ = workspace.open_settings(owner, Some(Section::AgentDesign));
    let id = workspace.settings.for_owner(owner).unwrap();
    let _ = workspace.update_settings(Message::AgentDesignCatalog(
        id,
        Edit::Preview(Some(super::super::Module::History(History::LogTable))),
    ));
    let dialog = workspace.settings.0.as_mut().unwrap();
    let demo = &mut dialog.agent_design.demo;
    while demo.active() {
        demo.update(Action::Next(demo.token()));
    }
    let token = demo.token();
    let mut measure = PreviewMeasure {
        page: dialog.page_scroll.clone(),
        offset: 0.0,
        visible_bottom: 0.0,
        answer_bottom: None,
        has_details: false,
    };
    let size = Size::new(880.0, 475.0);
    let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
    let mut ui = UserInterface::build(
        workspace.settings_view(id).unwrap(),
        size,
        Cache::default(),
        &mut renderer,
    );
    let press = |ui: &mut UserInterface<'_, Desktop, _, _>, renderer: &mut Renderer, key| {
        let mut messages = test_input::press(ui, renderer, Key::Named(key), None);
        assert_eq!(messages.len(), 1);
        messages.remove(0)
    };
    let tab = press(&mut ui, &mut renderer, Named::Tab);
    assert!(
        matches!(tab, Desktop::Settings(Message::AgentDesignCatalog(window, Edit::FocusPreview(false))) if window == id)
    );
    let cache = ui.into_cache();
    let task = workspace.update(tab);
    let mut ui = UserInterface::build(
        workspace.settings_view(id).unwrap(),
        size,
        cache,
        &mut renderer,
    );
    test_input::apply(&mut ui, &renderer, task);
    ui.operate(&renderer, &mut measure);
    assert!(
        measure.offset > 0.0,
        "focus reveals the preview in the Settings page"
    );

    for (key, expected) in [
        (Named::End, SampleLogKey::Last),
        (Named::Enter, SampleLogKey::Open),
    ] {
        let action = press(&mut ui, &mut renderer, key);
        assert!(
            matches!(action, Desktop::Settings(Message::AgentDesignCatalog(window, Edit::NavigatePreview(current, key))) if window == id && current == token && key == expected)
        );
        let cache = ui.into_cache();
        let task = workspace.update(action);
        ui = UserInterface::build(
            workspace.settings_view(id).unwrap(),
            size,
            cache,
            &mut renderer,
        );
        test_input::apply(&mut ui, &renderer, task);
    }
    ui.operate(&renderer, &mut measure);
    assert!(measure.has_details);
    assert!(
        measure
            .answer_bottom
            .is_some_and(|bottom| bottom <= measure.visible_bottom + 1.0),
        "opening a detail reveals its output within the settings page"
    );
    let escape = press(&mut ui, &mut renderer, Named::Escape);
    assert!(
        matches!(escape, Desktop::Settings(Message::AgentDesignCatalog(window, Edit::NavigatePreview(current, SampleLogKey::Close))) if window == id && current == token)
    );
    let cache = ui.into_cache();
    let _ = workspace.update(escape);
    let mut ui = UserInterface::build(
        workspace.settings_view(id).unwrap(),
        size,
        cache,
        &mut renderer,
    );
    measure.has_details = false;
    ui.operate(&renderer, &mut measure);
    assert!(!measure.has_details);
    drop(ui);

    let _ = workspace.update_settings(Message::AgentDesignCatalog(id, Edit::Demo(Action::Replay)));
    let stale = workspace.update_settings(Message::AgentDesignCatalog(
        id,
        Edit::NavigatePreview(token, SampleLogKey::Open),
    ));
    assert!(iced_runtime::task::into_stream(stale).is_none());
}
