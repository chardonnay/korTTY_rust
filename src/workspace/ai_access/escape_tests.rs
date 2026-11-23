use iced::advanced::widget::{Id, Operation, Tree, operation};
use iced::advanced::{Layout, clipboard, layout};
use iced::futures::{StreamExt, executor::block_on};
use iced::keyboard::{self, Location, key};
use iced::{Event, Font, Point, Rectangle, Renderer, Theme, event, mouse};
use iced_runtime::{Action, UserInterface, task, user_interface::Cache};

use super::*;

const SIZE: Size = Size::new(900.0, 650.0);

fn manager(hosted: bool) -> (Workspace, window::Id, window::Id) {
    let mut workspace = Workspace::default();
    workspace.window_settings.open_tool_windows_as_tabs = hosted;
    let (owner, _) = workspace.open_window(false);
    let dialog = Dialog::new(
        Arc::new(Configuration::default()),
        std::env::temp_dir().join("kortty-ai-escape-test-unused"),
        Vec::new(),
        true,
        None,
    );
    let _ = workspace.open_ai_manager_surface(owner, dialog);
    let id = workspace.ai_manager_id().unwrap();
    (workspace, owner, id)
}

fn key_press(key: Key, text: Option<&str>) -> Event {
    Event::Keyboard(keyboard::Event::KeyPressed {
        modified_key: key.clone(),
        key,
        physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
        location: Location::Standard,
        modifiers: Modifiers::empty(),
        text: text.map(Into::into),
        repeat: false,
    })
}

fn escape() -> Event {
    key_press(Key::Named(Named::Escape), None)
}

fn focus_next(interface: &mut UserInterface<'_, Desktop, Theme, Renderer>, renderer: &Renderer) {
    let mut next = operation::focusable::focus_next();
    interface.operate(renderer, &mut next);
    let operation::Outcome::Chain(mut focus) = next.finish() else {
        panic!("expected a focusable AI Manager field");
    };
    interface.operate(renderer, focus.as_mut());
}

fn dispatch(
    interface: &mut UserInterface<'_, Desktop, Theme, Renderer>,
    renderer: &mut Renderer,
    events: &[Event],
    cursor: mouse::Cursor,
) -> Vec<Desktop> {
    let mut messages = Vec::new();
    let (_, statuses) = interface.update(
        events,
        cursor,
        renderer,
        &mut clipboard::Null,
        &mut messages,
    );
    assert_eq!(statuses, vec![event::Status::Captured; events.len()]);
    messages
}

fn assert_escape(messages: &[Desktop], id: window::Id) {
    assert!(
        matches!(messages, [Desktop::AiManager(Message::Dialog(surface, action))]
        if *surface == id && matches!(action.as_ref(), ai_manager::Message::Escape))
    );
}

#[test]
fn escape_from_focused_ai_manager_field_uses_the_close_guard_in_window_and_tab() {
    for hosted in [false, true] {
        let (mut workspace, owner, id) = manager(hosted);
        let _ = workspace.update(wrap(
            id,
            ai_manager::Message::Search("global request timeout".into()),
        ));
        let focus = workspace.update(wrap(id, ai_manager::Message::SearchChoose));
        let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        let (cache, messages) = {
            let mut interface = UserInterface::build(
                workspace.ai_manager_view(id).unwrap(),
                SIZE,
                Cache::default(),
                &mut renderer,
            );
            crate::settings_search::test_input::apply(&mut interface, &renderer, focus);
            let messages = dispatch(
                &mut interface,
                &mut renderer,
                &[key_press(Key::Character("7".into()), Some("7")), escape()],
                mouse::Cursor::Unavailable,
            );
            (interface.into_cache(), messages)
        };
        let (edit, close) = messages.split_first().unwrap();
        assert!(
            matches!(edit, Desktop::AiManager(Message::Dialog(surface, action))
            if *surface == id && matches!(action.as_ref(), ai_manager::Message::Timeout(_)))
        );
        assert_escape(close, id);
        for message in messages {
            let _ = workspace.update(message);
        }
        assert!(workspace.ai_manager_needs_close(id));
        assert!(workspace.hosted_close_locks(id));
        assert!(workspace.windows.contains_key(&owner));

        let messages = {
            let mut interface = UserInterface::build(
                workspace.ai_manager_view(id).unwrap(),
                SIZE,
                cache,
                &mut renderer,
            );
            dispatch(
                &mut interface,
                &mut renderer,
                &[escape()],
                mouse::Cursor::Unavailable,
            )
        };
        assert_escape(&messages, id);
        let _ = workspace.update(messages.into_iter().next().unwrap());
        assert!(workspace.ai_manager_needs_close(id));
        assert!(!workspace.hosted_close_locks(id));
        assert!(workspace.windows.contains_key(&owner));
    }
}

#[test]
fn ai_manager_escape_clears_focused_settings_search_before_closing_the_window() {
    let (mut workspace, owner, id) = manager(false);
    let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
    let (cache, messages) = {
        let mut interface =
            UserInterface::build(workspace.view(id), SIZE, Cache::default(), &mut renderer);
        focus_next(&mut interface, &renderer);
        let messages = dispatch(
            &mut interface,
            &mut renderer,
            &[key_press(Key::Character("m".into()), Some("m")), escape()],
            mouse::Cursor::Unavailable,
        );
        (interface.into_cache(), messages)
    };
    let (search, escaped) = messages.split_first().unwrap();
    assert!(
        matches!(search, Desktop::AiManager(Message::Dialog(surface, action))
        if *surface == id && matches!(action.as_ref(), ai_manager::Message::Search(query) if query == "m"))
    );
    assert_escape(escaped, id);
    for message in messages {
        let _ = workspace.update(message);
    }
    assert!(workspace.ai_manager_open());
    assert!(!workspace.hosted_close_locks(id));

    let messages = {
        let mut interface = UserInterface::build(workspace.view(id), SIZE, cache, &mut renderer);
        dispatch(
            &mut interface,
            &mut renderer,
            &[escape()],
            mouse::Cursor::Unavailable,
        )
    };
    assert_escape(&messages, id);
    let _ = workspace.update(messages.into_iter().next().unwrap());
    assert!(!workspace.ai_manager_open());
    assert!(workspace.windows.contains_key(&owner));
}

#[test]
fn ai_manager_escape_dismisses_focused_document_find_before_requesting_close() {
    let (mut workspace, _, id) = manager(false);
    let (_, dialog) = workspace.ai_manager.surface.as_mut().unwrap();
    let mut focus = task::into_stream(dialog.open_skill_find_for_test()).unwrap();
    let Some(Action::Widget(mut operation)) = block_on(focus.next()) else {
        panic!("expected the document Find input to be focused");
    };

    let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
    let messages = {
        let mut interface =
            UserInterface::build(workspace.view(id), SIZE, Cache::default(), &mut renderer);
        interface.operate(&renderer, operation.as_mut());
        dispatch(
            &mut interface,
            &mut renderer,
            &[key_press(Key::Character("m".into()), Some("m")), escape()],
            mouse::Cursor::Unavailable,
        )
    };
    let (query, escaped) = messages.split_first().unwrap();
    assert!(
        matches!(query, Desktop::AiManager(Message::Dialog(surface, action))
        if *surface == id && matches!(action.as_ref(), ai_manager::Message::Skills(_)))
    );
    assert_escape(escaped, id);
    for message in messages {
        let _ = workspace.update(message);
    }
    let (_, dialog) = workspace.ai_manager.surface.as_ref().unwrap();
    assert!(matches!(
        dialog.escape_message(),
        ai_manager::Message::Close
    ));
    assert!(!workspace.hosted_close_locks(id));
}

#[test]
fn ai_manager_general_picker_consumes_escape_before_the_close_guard() {
    #[derive(Default)]
    struct PickerLabel(Option<Rectangle>);

    impl Operation for PickerLabel {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }

        fn text(&mut self, _id: Option<&Id>, bounds: Rectangle, label: &str) {
            if label.trim_end_matches(':') == "AI chat code blocks in the terminal" {
                self.0 = Some(bounds);
            }
        }
    }

    fn picker_center(layout: Layout<'_>, label: Rectangle) -> Option<Point> {
        let mut children = layout.children();
        if children.next().is_some_and(|child| child.bounds() == label) {
            return children.next().map(|child| child.bounds().center());
        }
        layout
            .children()
            .find_map(|child| picker_center(child, label))
    }

    let (workspace, _, id) = manager(false);
    let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
    let mut view = workspace.ai_manager_view(id).unwrap();
    let mut tree = Tree::new(view.as_widget());
    let node =
        view.as_widget_mut()
            .layout(&mut tree, &renderer, &layout::Limits::new(Size::ZERO, SIZE));
    let mut label = PickerLabel::default();
    view.as_widget_mut()
        .operate(&mut tree, Layout::new(&node), &renderer, &mut label);
    let label = label.0.expect("AI chat terminal actions label");
    let picker = picker_center(Layout::new(&node), label).expect("picker next to its label");
    let mut interface = UserInterface::build(view, SIZE, Cache::default(), &mut renderer);
    let messages = dispatch(
        &mut interface,
        &mut renderer,
        &[Event::Mouse(mouse::Event::ButtonPressed(
            mouse::Button::Left,
        ))],
        mouse::Cursor::Available(picker),
    );
    assert!(messages.is_empty());
    let messages = dispatch(
        &mut interface,
        &mut renderer,
        &[escape()],
        mouse::Cursor::Unavailable,
    );
    assert!(messages.is_empty());
    let messages = dispatch(
        &mut interface,
        &mut renderer,
        &[escape()],
        mouse::Cursor::Unavailable,
    );
    assert_escape(&messages, id);
}
