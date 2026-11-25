use super::*;
use crate::settings_search::test_input;
use iced::advanced::widget::{Operation, operation};
use iced::{Font, Rectangle, Renderer, Size, Vector};
use iced_runtime::{UserInterface, user_interface::Cache};

struct Viewports {
    jobs: Id,
    actions: Id,
    bounds: [Option<Rectangle>; 2],
}

impl Operation for Viewports {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn scrollable(
        &mut self,
        id: Option<&Id>,
        bounds: Rectangle,
        _: Rectangle,
        _: Vector,
        _: &mut dyn operation::Scrollable,
    ) {
        if id == Some(&self.jobs) {
            self.bounds[0] = Some(bounds);
        } else if id == Some(&self.actions) {
            self.bounds[1] = Some(bounds);
        }
    }
}

#[test]
fn wrapped_controls_and_long_status_leave_room_for_job_rows() {
    let profile = crate::scheduler_window::tests::Profile::temporary();
    let mut runtime = SchedulerWindow::open(profile.file()).unwrap();
    runtime.status = Some("Long retained status with enough text to wrap repeatedly. ".repeat(24));
    let mut view = runtime.new_view();
    view.dirty = true;
    view.status = Some("A second status message also needs room in the narrow pane.".repeat(12));
    for size in [
        Size::new(180.0, 230.0),
        Size::new(260.0, 360.0),
        Size::new(360.0, 660.0),
    ] {
        let mut renderer = Renderer::new(Font::DEFAULT, 13.0.into());
        let mut ui = UserInterface::build(
            view.jobs_view(&runtime, true, None),
            size,
            Cache::default(),
            &mut renderer,
        );
        let mut geometry = Viewports {
            jobs: view.layout.job_scroll.clone(),
            actions: view.layout.action_scroll.clone(),
            bounds: [None, None],
        };
        ui.operate(&renderer, &mut geometry);
        let [Some(jobs), Some(actions)] = geometry.bounds else {
            panic!("Both panes must remain scrollable");
        };
        assert!(jobs.height >= size.height * 0.25, "{size:?}: {jobs:?}");
        assert!(
            jobs.width >= 640.0,
            "Columns must remain readable: {jobs:?}"
        );
        assert!(actions.height > 0.0 && actions.height <= size.height * 0.45);
        assert!(jobs.y + jobs.height <= actions.y);
        assert!(actions.y + actions.height <= size.height);
    }
}

#[test]
fn job_arrows_and_keyboard_actions_keep_their_distinct_meaning() {
    use iced::keyboard::{Key, key::Named};
    let profile = crate::scheduler_window::tests::Profile::temporary();
    let jobs: Vec<_> = ["first", "second"]
        .into_iter()
        .map(|name| {
            let action: SshCommand = serde_json::from_value(serde_json::json!({
                "command": "true",
                "host": "layout.example",
                "port": 22,
                "username": "operator",
                "key_file": "/unused-layout-fixture-key",
            }))
            .unwrap();
            kortty_jobs::Job::new(JobId::new(name).unwrap(), name, action)
                .unwrap()
                .with_enabled(false)
        })
        .collect();
    kortty_jobs::SchedulerStore::open(profile.file())
        .unwrap()
        .commit_with_actions(&kortty_jobs::JobGraph::new(jobs).unwrap(), &[], &[])
        .unwrap();
    let runtime = SchedulerWindow::open(profile.file()).unwrap();
    let view = runtime.new_view();
    let mut renderer = Renderer::new(Font::DEFAULT, 13.0.into());
    let mut ui = UserInterface::build(
        view.view(&runtime, true, false, None, Vec::new()),
        Size::new(700.0, 440.0),
        Cache::default(),
        &mut renderer,
    );
    test_input::apply(
        &mut ui,
        &renderer,
        iced::widget::operation::focus(view.layout.job_table.clone()),
    );
    assert!(
        matches!(test_input::press(&mut ui, &mut renderer, Key::Named(Named::ArrowDown), None).as_slice(),
        [Message::Select(id)] if id.as_str() == "second")
    );
    test_input::apply(
        &mut ui,
        &renderer,
        iced_runtime::task::effect(iced_runtime::Action::widget(operation::scope(
            view.layout.scope.clone(),
            operation::focusable::focus_next(),
        ))),
    );
    assert!(matches!(
        test_input::press(&mut ui, &mut renderer, Key::Named(Named::Enter), None).as_slice(),
        [Message::New]
    ));
    test_input::apply(
        &mut ui,
        &renderer,
        iced_runtime::task::effect(iced_runtime::Action::widget(operation::scope(
            view.layout.scope.clone(),
            operation::focusable::focus_next(),
        ))),
    );
    assert!(matches!(
        test_input::press(&mut ui, &mut renderer, Key::Named(Named::Enter), None).as_slice(),
        [Message::Save]
    ));
    assert!(matches!(
        test_input::press(&mut ui, &mut renderer, Key::Named(Named::Tab), None).as_slice(),
        [Message::Traverse(false)]
    ));
    let mut messages = Vec::new();
    let _ = ui.update(
        &[iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
            key: Key::Named(Named::Tab),
            modified_key: Key::Named(Named::Tab),
            physical_key: iced::keyboard::key::Physical::Unidentified(
                iced::keyboard::key::NativeCode::Unidentified,
            ),
            location: iced::keyboard::Location::Standard,
            modifiers: iced::keyboard::Modifiers::CTRL,
            text: None,
            repeat: false,
        })],
        iced::mouse::Cursor::Unavailable,
        &mut renderer,
        &mut iced::advanced::clipboard::Null,
        &mut messages,
    );
    assert!(
        messages.is_empty(),
        "Ctrl+Tab belongs to window/tab navigation"
    );
}
