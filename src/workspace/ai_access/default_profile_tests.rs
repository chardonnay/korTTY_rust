use iced::advanced::{
    clipboard,
    widget::{Id, Operation},
};
use iced::futures::{StreamExt as _, executor::block_on};
use iced::{Event, Font, Rectangle, Renderer, mouse};
use iced_runtime::{UserInterface, user_interface::Cache};
use serde_json::json;

use crate::ai_manager::{Page, ProfileMessageForTest};
use crate::snippets::tests::Profile as Scratch;

use super::*;

#[derive(Default)]
struct DefaultCheckbox(Option<Rectangle>);

impl Operation for DefaultCheckbox {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn text(&mut self, _id: Option<&Id>, bounds: Rectangle, label: &str) {
        if label == "Use as default profile" {
            self.0 = Some(bounds);
        }
    }
}

#[test]
fn saved_ai_manager_default_reaches_live_routing_and_survives_draft_discard() {
    for hosted in [false, true] {
        let scratch = Scratch::new();
        let root = scratch.0.canonicalize().unwrap();
        let configured: Configuration = serde_json::from_value(json!({
            "profiles": {
                "profiles": [
                    {"id": 1, "name": "First profile", "endpoint": "127.0.0.1:8081", "model": "first"},
                    {"id": 2, "name": "Second profile", "endpoint": "127.0.0.1:8082", "model": "second"}
                ],
                "default": 1,
                "last_id": 2
            }
        }))
        .unwrap();
        configured
            .save_fixture_at(&root, &Configuration::default())
            .unwrap();

        let mut workspace = Workspace::default();
        workspace.window_settings.open_tool_windows_as_tabs = hosted;
        workspace.ai_manager.config = Arc::new(Configuration::load_fixture_from(&root).unwrap());
        let (owner, _) = workspace.open_window(false);
        let open = |workspace: &mut Workspace| {
            let dialog = Dialog::new(
                Arc::clone(&workspace.ai_manager.config),
                root.clone(),
                Vec::new(),
                true,
                None,
            );
            let _ = workspace.open_ai_manager_surface(owner, dialog);
            workspace.ai_manager_id().unwrap()
        };
        let id = open(&mut workspace);
        assert_eq!(workspace.hosted_tool_owner(id), hosted.then_some(owner));
        assert_eq!(
            workspace.ai_manager.config.terminal_chat_profile_id(),
            Some(1)
        );

        let _ = workspace.update(wrap(id, ai_manager::Message::Features(false)));
        let _ = workspace.update(wrap(id, ai_manager::Message::Page(Page::Profiles)));
        let _ = workspace.update(wrap(
            id,
            ai_manager::Message::Profiles(ProfileMessageForTest::Select(2)),
        ));
        let event = {
            let size = Size::new(760.0, 480.0);
            let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
            let mut interface = UserInterface::build(
                workspace.ai_manager_view(id).unwrap(),
                size,
                Cache::default(),
                &mut renderer,
            );
            let mut checkbox = DefaultCheckbox::default();
            interface.operate(&renderer, &mut checkbox);
            let bounds = checkbox.0.expect("laid-out default profile checkbox");
            assert!(bounds.width > 0.0 && bounds.height > 0.0);
            let cursor = bounds.center();
            assert!(Rectangle::with_size(size).contains(cursor));
            let mut messages = Vec::new();
            let _ = interface.update(
                &[Event::Mouse(mouse::Event::ButtonPressed(
                    mouse::Button::Left,
                ))],
                mouse::Cursor::Available(cursor),
                &mut renderer,
                &mut clipboard::Null,
                &mut messages,
            );
            assert_eq!(messages.len(), 1);
            let message = messages.pop().unwrap();
            assert!(matches!(
                &message,
                Desktop::AiManager(Message::Dialog(target, event))
                    if *target == id
                        && matches!(&**event, ai_manager::Message::Profiles(ProfileMessageForTest::SetDefault(2)))
            ));
            message
        };
        let write = workspace.update(event);
        assert_eq!(
            workspace.ai_manager.config.terminal_chat_profile_id(),
            Some(1)
        );
        let mut actions = iced_runtime::task::into_stream(write).expect("save task");
        while let Some(action) = block_on(actions.next()) {
            if let iced_runtime::Action::Output(event @ Desktop::AiManager(_)) = action {
                let _ = workspace.update(event);
            }
        }

        assert_eq!(
            workspace.ai_manager.config.terminal_chat_profile_id(),
            Some(2)
        );
        assert!(workspace.ai_features_enabled());
        assert!(workspace.ai_manager_needs_close(id));
        let saved = Configuration::load_fixture_from(&root).unwrap();
        assert_eq!(saved.terminal_chat_profile_id(), Some(2));
        assert!(saved.enabled());

        let _ = workspace.update(wrap(id, ai_manager::Message::Close));
        assert!(workspace.hosted_close_locks(id));
        let _ = workspace.update(wrap(id, ai_manager::Message::CloseAnswer(false)));
        assert!(workspace.ai_manager_needs_close(id));
        let _ = workspace.update(wrap(id, ai_manager::Message::Close));
        let _ = workspace.update(wrap(id, ai_manager::Message::CloseAnswer(true)));
        assert!(!workspace.ai_manager_open());

        workspace.ai_manager.config = Arc::new(Configuration::load_fixture_from(&root).unwrap());
        let reopened = open(&mut workspace);
        assert_eq!(
            workspace.ai_manager.config.terminal_chat_profile_id(),
            Some(2)
        );
        assert!(workspace.ai_features_enabled());
        assert!(!workspace.ai_manager_needs_close(reopened));
    }
}
