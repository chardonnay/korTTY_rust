use super::*;

#[test]
fn transparency_draft_requires_save_and_failed_save_keeps_the_live_value() {
    if !super::super::transparency::platform_candidate() {
        return;
    }
    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    let _ = workspace.open_settings(owner, Some(Section::Font));
    let id = workspace.settings.for_owner(owner).unwrap();
    let _ = workspace.update_settings(Message::Transparency(id, 20));
    assert_eq!(
        workspace
            .settings
            .0
            .as_ref()
            .unwrap()
            .draft
            .preferences
            .terminal_transparency,
        0
    );
    let _ = workspace.update_settings(Message::Section(id, Section::Window));
    let _ = workspace.update_settings(Message::Transparency(id, 255));
    assert_eq!(
        workspace
            .settings
            .0
            .as_ref()
            .unwrap()
            .draft
            .preferences
            .terminal_transparency,
        100
    );
    assert_eq!(workspace.window_settings.terminal_transparency, 0);
    assert!(!workspace.transparency_save_pending);
    let _ = workspace.save_settings_to(id, |_| Err(std::io::Error::other("fixture write failure")));
    assert_eq!(workspace.window_settings.terminal_transparency, 0);
    assert!(!workspace.windows[&owner].transparency.requested);
    assert!(
        workspace
            .settings
            .0
            .as_ref()
            .unwrap()
            .problem
            .as_deref()
            .unwrap()
            .contains("fixture write failure")
    );
    let _ = workspace.update_settings(Message::Cancel(id));
    let _ = workspace.update_settings(Message::Transparency(id, 30));
    assert_eq!(workspace.window_settings.terminal_transparency, 0);

    let _ = workspace.open_settings(owner, Some(Section::Window));
    let id = workspace.settings.for_owner(owner).unwrap();
    let _ = workspace.update_settings(Message::Transparency(id, 35));
    let _ = workspace.save_settings_to(id, |next| {
        assert_eq!(next.terminal_transparency, 35);
        Ok(())
    });
    assert_eq!(workspace.window_settings.terminal_transparency, 35);
    assert!(!workspace.transparency_save_pending);
    assert_eq!(
        workspace.windows[&owner].transparency.requested,
        cfg!(target_os = "windows")
    );
    let (next, _) = workspace.open_window(false);
    assert!(workspace.windows[&next].transparency.requested);
}

#[test]
fn transparency_search_focuses_the_slider_and_supports_bounded_keyboard_steps() {
    use crate::settings_search::test_input;
    use iced::{Font, Renderer};
    use iced_runtime::{UserInterface, user_interface::Cache};

    if !super::super::transparency::platform_candidate() {
        return;
    }
    for (key, expected) in [
        (Named::ArrowLeft, 19),
        (Named::ArrowDown, 19),
        (Named::ArrowRight, 21),
        (Named::ArrowUp, 21),
        (Named::Home, 0),
        (Named::End, 100),
    ] {
        let mut workspace = Workspace::default();
        workspace.window_settings.terminal_transparency = 20;
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.open_settings(owner, Some(Section::Font));
        let id = workspace.settings.for_owner(owner).unwrap();
        let _ = workspace.update_settings(Message::Search(id, "transparency".into()));
        let reveal = workspace.update_settings(Message::SearchChoose(id));
        assert_eq!(
            workspace.settings.0.as_ref().unwrap().revealed,
            Some(Setting::Transparency)
        );
        let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        let mut interface = UserInterface::build(
            workspace.settings_view(id).unwrap(),
            Size::new(620.0, 430.0),
            Cache::default(),
            &mut renderer,
        );
        test_input::apply(&mut interface, &renderer, reveal);
        let messages = test_input::press(&mut interface, &mut renderer, Key::Named(key), None);
        assert!(
            matches!(messages.as_slice(), [Desktop::Settings(Message::Transparency(target, value))] if *target == id && *value == expected)
        );
        drop(interface);
        for message in messages {
            let _ = workspace.update(message);
        }
        assert_eq!(
            workspace
                .settings
                .0
                .as_ref()
                .unwrap()
                .draft
                .preferences
                .terminal_transparency,
            expected
        );
        assert_eq!(workspace.window_settings.terminal_transparency, 20);
        assert!(!workspace.transparency_save_pending);
    }
}

#[test]
fn unrelated_settings_save_preserves_the_live_quick_menu_transparency() {
    if !super::super::transparency::platform_candidate() {
        return;
    }
    let mut workspace = Workspace::default();
    workspace.window_settings.terminal_transparency = 20;
    let (owner, _) = workspace.open_window(false);
    let _ = workspace.open_settings(owner, Some(Section::Font));
    let id = workspace.settings.for_owner(owner).unwrap();
    let _ = workspace.update_transparency(super::super::transparency::Message::Set(72));
    assert_eq!(workspace.window_settings.terminal_transparency, 72);
    assert!(workspace.transparency_save_pending);
    let _ = workspace.update_settings(Message::FontSize(id, 19));
    let _ = workspace.save_settings_to(id, |next| {
        assert_eq!(next.terminal_transparency, 72);
        assert_eq!(next.terminal_font, 19);
        Ok(())
    });
    assert_eq!(workspace.window_settings.terminal_transparency, 72);
    assert!(workspace.transparency_save_pending);
}
