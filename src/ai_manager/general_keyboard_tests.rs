use super::*;
use crate::settings_search::test_input;
use iced::keyboard::{Key, key::Named};
use iced::{Font, Renderer, Size};
use iced_runtime::{UserInterface, user_interface::Cache};
use search::{ImportedGeneral, Setting};

#[test]
fn agent_message_preferences_are_saved_drafts_with_keyboard_and_readonly_guards() {
    let legacy = Configuration::decode(b"{}").unwrap();
    assert!(!legacy.agent_debug_messages() && !legacy.agent_runtime_messages());
    let profile = crate::snippets::tests::Profile::new();
    let mut dialog = Dialog::new(Arc::new(legacy), profile.0.clone(), Vec::new(), true, None);
    for setting in [Setting::AgentDebugMessages, Setting::AgentRuntimeMessages] {
        let messages = search_and_press(&mut dialog, setting, &[Named::Space]);
        assert!(matches!(
            messages.as_slice(),
            [Message::AgentDebugMessages(true)] | [Message::AgentRuntimeMessages(true)]
        ));
        for message in messages {
            let _ = dialog.update(message);
        }
    }
    assert!(!dialog.saved.agent_debug_messages() && !dialog.saved.agent_runtime_messages());
    save_security_dialog(&mut dialog);
    let saved = Configuration::load_fixture_from(&profile.0).unwrap();
    assert!(saved.agent_debug_messages() && saved.agent_runtime_messages());
    let mut readonly = Dialog::new(Arc::new(saved), profile.0.clone(), Vec::new(), false, None);
    let _ = readonly.update(Message::AgentDebugMessages(false));
    let _ = readonly.update(Message::AgentRuntimeMessages(false));
    assert!(readonly.general.agent_debug_messages && readonly.general.agent_runtime_messages);
}

#[test]
fn terminal_request_confirmation_defaults_to_review_and_persists_an_explicit_choice() {
    assert!(Configuration::decode(b"{}").unwrap().confirm_before_send());
    let profile = crate::snippets::tests::Profile::new();
    let mut dialog = Dialog::new(
        Arc::new(Configuration::default()),
        profile.0.clone(),
        Vec::new(),
        true,
        None,
    );
    let messages = search_and_press(&mut dialog, Setting::ConfirmBeforeSend, &[Named::Space]);
    assert!(matches!(
        messages.as_slice(),
        [Message::ConfirmBeforeSend(false)]
    ));
    for message in messages {
        let _ = dialog.update(message);
    }
    assert!(dialog.saved.confirm_before_send());
    save_security_dialog(&mut dialog);
    assert!(
        !Configuration::load_fixture_from(&profile.0)
            .unwrap()
            .confirm_before_send()
    );
    let mut readonly = Dialog::new(
        dialog.saved.clone(),
        profile.0.clone(),
        Vec::new(),
        false,
        None,
    );
    let _ = readonly.update(Message::ConfirmBeforeSend(true));
    assert!(!readonly.general.confirm_before_send);
}

#[test]
fn general_commands_precede_text_input_even_when_native_events_include_text() {
    use iced::advanced::clipboard;
    use iced::keyboard::{Event, Location, Modifiers, key};
    let profile = crate::snippets::tests::Profile::new();
    for command in ["s", "f", "w"] {
        let mut dialog = Dialog::new(
            Arc::new(Configuration::default()),
            profile.0.clone(),
            Vec::new(),
            true,
            None,
        );
        let reveal = dialog.update(Message::Reveal(search::Target::Setting(
            Setting::AgentHistoryLimit,
        )));
        let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        let mut ui = UserInterface::build(
            dialog.view(Vec::new(), Vec::new()),
            Size::new(760.0, 480.0),
            Cache::default(),
            &mut renderer,
        );
        test_input::apply(&mut ui, &renderer, reveal);
        let mut messages = Vec::new();
        let (_, statuses) = ui.update(
            &[iced::Event::Keyboard(Event::KeyPressed {
                key: Key::Character(command.into()),
                modified_key: Key::Character(command.into()),
                physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
                location: Location::Standard,
                modifiers: Modifiers::COMMAND,
                text: Some(command.into()),
                repeat: false,
            })],
            iced::mouse::Cursor::Unavailable,
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        assert_eq!(statuses, [iced::event::Status::Captured]);
        assert!(matches!(
            (command, messages.as_slice()),
            ("s", [Message::Save]) | ("f", [Message::Find]) | ("w", [Message::Close])
        ));
        drop(ui);
        assert_eq!(dialog.shared_defaults.history_limit, "20");
    }
}

#[test]
fn general_shared_defaults_search_and_save_use_the_existing_files() {
    let root = crate::snippets::tests::Profile::new();
    let mut dialog = Dialog::new(
        Arc::new(Configuration::default()),
        root.0.clone(),
        Vec::new(),
        true,
        None,
    );
    let messages = search_and_press(&mut dialog, Setting::SnippetInstructions, &[Named::Space]);
    assert!(matches!(
        messages.as_slice(),
        [Message::SnippetInstructions(true)]
    ));
    for message in messages {
        let _ = dialog.update(message);
    }
    let messages = search_and_press(
        &mut dialog,
        Setting::SnippetAlternatives,
        &[Named::Space, Named::End, Named::Enter],
    );
    assert!(matches!(
        messages.as_slice(),
        [Message::SnippetAlternatives(10)]
    ));
    for message in messages {
        let _ = dialog.update(message);
    }
    let _ = dialog.update(Message::AgentHistoryLimit("5".into()));
    assert!(dialog.dirty());
    assert!(!crate::profile::window_settings_path_at(&root.0).exists());
    save_security_dialog(&mut dialog);
    assert!(!dialog.dirty(), "{}", dialog.notice);
    let saved: serde_json::Value = serde_json::from_slice(
        &std::fs::read(crate::profile::window_settings_path_at(&root.0)).unwrap(),
    )
    .unwrap();
    assert_eq!(saved["snippet_ai_additional_instructions"], true);
    assert_eq!(saved["snippet_alternative_count"], 10);
    assert_eq!(
        crate::terminal_agent::history::list(&root.0, false)
            .unwrap()
            .limit,
        5
    );
    let reopened = Dialog::new(dialog.saved.clone(), root.0.clone(), Vec::new(), true, None);
    assert_eq!(reopened.shared_defaults.history_limit, "5");
    assert_eq!(
        reopened.shared_defaults.snippets().unwrap().alternatives,
        10
    );
}

#[test]
fn general_shared_defaults_reject_invalid_input_and_readonly_changes() {
    let profile = crate::snippets::tests::Profile::new();
    let mut dialog = Dialog::new(
        Arc::new(Configuration::default()),
        profile.0.clone(),
        Vec::new(),
        true,
        None,
    );
    let _ = dialog.update(Message::AgentHistoryLimit("101".into()));
    let _ = dialog.update(Message::Save);
    assert!(!dialog.saving);
    assert_eq!(dialog.revealed.setting, Some(Setting::AgentHistoryLimit));
    assert!(dialog.notice.contains("5 to 100"));
    let mut dialog = Dialog::new(
        Arc::new(Configuration::default()),
        profile.0.clone(),
        Vec::new(),
        false,
        None,
    );
    let before = dialog.shared_defaults.snippets();
    let _ = dialog.update(Message::SnippetInstructions(true));
    let _ = dialog.update(Message::SnippetAlternatives(10));
    let _ = dialog.update(Message::AgentHistoryLimit("5".into()));
    assert_eq!(dialog.shared_defaults.snippets(), before);
    assert_eq!(dialog.shared_defaults.history_limit, "20");
    assert!(!dialog.dirty());
}

fn dialog(editable: bool) -> Dialog {
    Dialog::new(
        Arc::new(Configuration::default()),
        PathBuf::new(),
        Vec::new(),
        editable,
        None,
    )
}

fn search_and_press(dialog: &mut Dialog, setting: Setting, keys: &[Named]) -> Vec<Message> {
    let before = dialog.general.clone();
    let _ = dialog.update(Message::Search(setting.title().into()));
    let reveal = dialog.update(Message::SearchChoose);
    assert_eq!(dialog.revealed.setting, Some(setting));
    assert_eq!(
        dialog.general, before,
        "search must not adopt or edit a setting"
    );
    let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
    let mut ui = UserInterface::build(
        dialog.view(Vec::new(), Vec::new()),
        Size::new(760.0, 480.0),
        Cache::default(),
        &mut renderer,
    );
    ui.operate(
        &renderer,
        &mut iced::advanced::widget::operation::focusable::focus(dialog.search_id.clone()),
    );
    test_input::apply(&mut ui, &renderer, reveal);
    keys.iter()
        .flat_map(|key| {
            test_input::press(
                &mut ui,
                &mut renderer,
                Key::Named(*key),
                (*key == Named::Space).then_some(" "),
            )
        })
        .collect()
}

#[test]
fn general_search_toggles_the_exact_focused_switch() {
    for setting in [
        Setting::Features,
        Setting::AgentShortcut,
        Setting::AgentShortcutCase,
        Setting::AgentExecution,
        Setting::AgentConfirmation,
        Setting::ShowAgentSetup,
        Setting::JournalSemanticSearch,
    ] {
        let mut dialog = dialog(true);
        let before = dialog.general.clone();
        let messages = search_and_press(&mut dialog, setting, &[Named::Space]);
        match (setting, messages.as_slice()) {
            (Setting::Features, [Message::Features(value)]) => {
                assert_eq!(*value, !before.features_enabled)
            }
            (Setting::AgentShortcut, [Message::AgentShortcutEnabled(value)]) => {
                assert_eq!(*value, !before.agent_shortcut.enabled)
            }
            (Setting::AgentShortcutCase, [Message::AgentShortcutCaseInsensitive(value)]) => {
                assert_eq!(*value, !before.agent_shortcut.case_insensitive)
            }
            (Setting::AgentExecution, [Message::AgentExecutionEnabled(value)]) => {
                assert_eq!(*value, !before.agent_execution_enabled)
            }
            (Setting::AgentConfirmation, [Message::AgentConfirmMutating(value)]) => {
                assert_eq!(*value, !before.agent_confirm_mutating)
            }
            (Setting::ShowAgentSetup, [Message::ShowAgentSetup(value)]) => {
                assert_eq!(*value, !before.show_agent_setup)
            }
            (Setting::JournalSemanticSearch, [Message::JournalSemanticSearch(value)]) => {
                assert_eq!(*value, !before.journal_semantic_search)
            }
            _ => panic!("wrong switch dispatched for {setting:?}: {messages:?}"),
        }
        assert_eq!(dialog.general, before);
        for message in messages {
            let _ = dialog.update(message);
        }
        assert_ne!(dialog.general, before);
        assert_eq!(
            dialog.saved.general, before,
            "keyboard changes remain a draft"
        );
    }
}

#[test]
fn general_search_selectors_open_and_commit_with_the_keyboard() {
    for setting in [
        Setting::CodeTextLanguage,
        Setting::ChatTerminalActions,
        Setting::AgentTriage,
        Setting::AgentRunPresentation,
    ] {
        let mut dialog = dialog(true);
        let messages = search_and_press(
            &mut dialog,
            setting,
            &[Named::Space, Named::Home, Named::ArrowDown, Named::Enter],
        );
        match (setting, messages.as_slice()) {
            (Setting::CodeTextLanguage, [Message::CodeTextLanguage(CodeTextLanguage::English)])
            | (
                Setting::ChatTerminalActions,
                [Message::ChatTerminalActions(ChatTerminalActions::InsertOnly)],
            )
            | (
                Setting::AgentTriage,
                [Message::AgentTriage(crate::terminal_agent::planning::TriageMode::Always)],
            )
            | (
                Setting::AgentRunPresentation,
                [Message::AgentRunPresentation(crate::terminal_agent::RunPresentation::ChatTab)],
            ) => {}
            _ => panic!("wrong selection for {setting:?}: {messages:?}"),
        }
        let before = dialog.general.clone();
        for message in messages {
            let _ = dialog.update(message);
        }
        assert_ne!(dialog.general, before);
        assert_eq!(dialog.saved.general, before);
    }
}

#[test]
fn readonly_and_managed_general_controls_do_not_acquire_an_edit_action() {
    for setting in [
        Setting::Features,
        Setting::AgentShortcutCase,
        Setting::CodeTextLanguage,
        Setting::AgentTriage,
    ] {
        let mut dialog = dialog(false);
        let before = dialog.general.clone();
        let messages = search_and_press(&mut dialog, setting, &[Named::Space]);
        assert!(
            messages
                .iter()
                .all(|message| matches!(message, Message::Search(_)))
        );
        assert_eq!(dialog.general, before);
    }
    let mut dialog = dialog(true);
    dialog.managed_triage = Some(crate::terminal_agent::planning::TriageMode::Always);
    let before = dialog.general.clone();
    let messages = search_and_press(&mut dialog, Setting::AgentTriage, &[Named::Space]);
    assert!(
        messages
            .iter()
            .all(|message| matches!(message, Message::Search(_)))
    );
    assert_eq!(dialog.general, before);
}

#[test]
fn imported_general_preferences_need_explicit_keyboard_adoption() {
    for setting in [
        ImportedGeneral::JournalSearch,
        ImportedGeneral::AgentConfirmation,
        ImportedGeneral::AgentExecution,
        ImportedGeneral::AgentShortcut,
    ] {
        let config = Configuration {
            java_journal_semantic_search: Some(true),
            java_agent_confirm_mutating: Some(true),
            java_agent_execution_enabled: Some(false),
            java_agent_shortcut: Some(JavaAgentShortcut {
                name: Some("fixture".into()),
                case_insensitive: Some(false),
            }),
            ..Configuration::default()
        };
        let mut dialog = Dialog::new(Arc::new(config), PathBuf::new(), Vec::new(), true, None);
        let before = dialog.general.clone();
        let messages = search_and_press(
            &mut dialog,
            Setting::ImportedGeneral(setting),
            &[Named::Enter],
        );
        match (setting, messages.as_slice()) {
            (ImportedGeneral::JournalSearch, [Message::AdoptJavaJournalSemanticSearch])
            | (ImportedGeneral::AgentConfirmation, [Message::AdoptJavaAgentConfirmation])
            | (ImportedGeneral::AgentExecution, [Message::AdoptJavaAgentExecution])
            | (ImportedGeneral::AgentShortcut, [Message::AdoptJavaAgentShortcut]) => {}
            _ => panic!("wrong adoption for {setting:?}: {messages:?}"),
        }
        for message in messages {
            let _ = dialog.update(message);
        }
        assert_ne!(dialog.general, before);
        assert_eq!(dialog.saved.general, before);
    }
}

#[test]
fn general_tab_navigation_moves_between_fields_and_reaches_save() {
    let profile = crate::snippets::tests::Profile::new();
    let mut dialog = Dialog::new(
        Arc::new(Configuration::default()),
        profile.0.clone(),
        Vec::new(),
        true,
        None,
    );
    let _ = dialog.update(Message::Search(Setting::CodeTextLanguage.title().into()));
    let reveal = dialog.update(Message::SearchChoose);
    let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
    let mut ui = UserInterface::build(
        dialog.view(Vec::new(), Vec::new()),
        Size::new(760.0, 480.0),
        Cache::default(),
        &mut renderer,
    );
    test_input::apply(&mut ui, &renderer, reveal);
    test_input::apply(&mut ui, &renderer, dialog.focus_general_next(true).unwrap());
    let messages = test_input::press(
        &mut ui,
        &mut renderer,
        Key::Character("2".into()),
        Some("2"),
    );
    assert!(matches!(messages.as_slice(), [Message::Timeout(value)] if value == "12"));
    test_input::apply(
        &mut ui,
        &renderer,
        dialog.focus_general_next(false).unwrap(),
    );
    test_input::apply(
        &mut ui,
        &renderer,
        dialog.focus_general_next(false).unwrap(),
    );
    let mut changed = test_input::press(&mut ui, &mut renderer, Key::Named(Named::Space), None);
    assert!(matches!(
        changed.as_slice(),
        [Message::SnippetInstructions(true)]
    ));
    test_input::apply(
        &mut ui,
        &renderer,
        dialog.focus_general_next(false).unwrap(),
    );
    let _ = test_input::press(&mut ui, &mut renderer, Key::Named(Named::Space), None);
    let _ = test_input::press(&mut ui, &mut renderer, Key::Named(Named::End), None);
    let alternatives = test_input::press(&mut ui, &mut renderer, Key::Named(Named::Enter), None);
    assert!(matches!(
        alternatives.as_slice(),
        [Message::SnippetAlternatives(10)]
    ));
    changed.extend(alternatives);
    test_input::apply(
        &mut ui,
        &renderer,
        dialog.focus_general_next(false).unwrap(),
    );
    let messages = test_input::press(&mut ui, &mut renderer, Key::Named(Named::Space), None);
    assert!(matches!(
        messages.as_slice(),
        [Message::JournalSemanticSearch(true)]
    ));
    changed.extend(messages);
    let cache = ui.into_cache();
    for message in changed {
        let _ = dialog.update(message);
    }
    let mut ui = UserInterface::build(
        dialog.view(Vec::new(), Vec::new()),
        Size::new(760.0, 480.0),
        cache,
        &mut renderer,
    );
    test_input::apply(
        &mut ui,
        &renderer,
        dialog.focus_general_next(false).unwrap(),
    );
    let messages = test_input::press(&mut ui, &mut renderer, Key::Named(Named::Enter), None);
    assert!(matches!(messages.as_slice(), [Message::Save]));
}

#[test]
fn general_close_review_keeps_keyboard_focus_inside_the_review() {
    let mut dialog = dialog(true);
    dialog.search = "pending search".into();
    dialog.prompt_close(Some(1));
    let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
    let mut ui = UserInterface::build(
        dialog.view(Vec::new(), Vec::new()),
        Size::new(760.0, 480.0),
        Cache::default(),
        &mut renderer,
    );
    test_input::apply(
        &mut ui,
        &renderer,
        dialog.focus_general_next(false).unwrap(),
    );
    let messages = test_input::press(&mut ui, &mut renderer, Key::Named(Named::Space), None);
    assert!(matches!(messages.as_slice(), [Message::CloseAnswer(false)]));
    test_input::apply(
        &mut ui,
        &renderer,
        dialog.focus_general_next(false).unwrap(),
    );
    let messages = test_input::press(&mut ui, &mut renderer, Key::Named(Named::Space), None);
    assert!(matches!(messages.as_slice(), [Message::CloseAnswer(true)]));
    test_input::apply(
        &mut ui,
        &renderer,
        dialog.focus_general_next(false).unwrap(),
    );
    let messages = test_input::press(&mut ui, &mut renderer, Key::Named(Named::Space), None);
    assert!(matches!(messages.as_slice(), [Message::CloseAnswer(false)]));
    test_input::apply(&mut ui, &renderer, dialog.focus_general_next(true).unwrap());
    let messages = test_input::press(&mut ui, &mut renderer, Key::Named(Named::Space), None);
    assert!(matches!(messages.as_slice(), [Message::CloseAnswer(true)]));
    drop(ui);
    dialog.page = Page::Memory;
    assert!(dialog.focus_general_next(false).is_none());
}

fn security_profiles() -> Configuration {
    Configuration::decode(br#"{
        "profiles": {
            "profiles": [
                {"id": 11, "name": "Default fixture", "endpoint": "127.0.0.1:11434", "model": "default"},
                {"id": 12, "name": "Security fixture", "endpoint": "127.0.0.1:11435", "model": "security"}
            ],
            "default": 11,
            "last_id": 12
        },
        "local": {"roles": [11, null, null, null]}
    }"#).unwrap()
}

fn save_security_dialog(dialog: &mut Dialog) {
    use iced::futures::{StreamExt, executor::block_on};
    let task = dialog.update(Message::Save);
    let mut outcomes = iced_runtime::task::into_stream(task)
        .unwrap_or_else(|| panic!("save did not start: {}", dialog.notice));
    let Some(iced_runtime::Action::Output(event)) = block_on(outcomes.next()) else {
        panic!("expected the saved settings result");
    };
    assert!(matches!(&event, Message::Saved(Ok(_))), "{event:?}");
    let _ = dialog.update(event);
}

#[test]
fn general_security_profile_search_selects_and_clears_the_existing_role() {
    let mut dialog = Dialog::new(
        Arc::new(security_profiles()),
        PathBuf::new(),
        Vec::new(),
        true,
        None,
    );
    let messages = search_and_press(
        &mut dialog,
        Setting::SecurityProfile,
        &[Named::Space, Named::End, Named::Enter],
    );
    assert!(matches!(
        messages.as_slice(),
        [Message::Local(local::Message::Role(
            local::Role::Security,
            Some(ProfileRef::Numeric(12))
        ))]
    ));
    for message in messages {
        let _ = dialog.update(message);
    }
    assert_eq!(
        dialog.local_state.profile(local::Role::Security),
        Some(&ProfileRef::Numeric(12))
    );
    assert_eq!(dialog.saved.local.profile(local::Role::Security), None);
    assert_eq!(
        dialog.local_state.profile(local::Role::Text),
        Some(&ProfileRef::Numeric(11))
    );
    assert_eq!(dialog.general_section, GeneralSection::Defaults);

    let messages = search_and_press(&mut dialog, Setting::ClearSecurityProfile, &[Named::Enter]);
    assert!(matches!(
        messages.as_slice(),
        [Message::Local(local::Message::Role(
            local::Role::Security,
            None
        ))]
    ));
    for message in messages {
        let _ = dialog.update(message);
    }
    assert_eq!(dialog.local_state.profile(local::Role::Security), None);
    let _ = dialog.update(Message::Search(
        Setting::ClearSecurityProfile.title().into(),
    ));
    assert!(
        !dialog
            .search_results()
            .any(|target| target == search::Target::Setting(Setting::ClearSecurityProfile))
    );
}

#[test]
fn general_security_profile_save_controls_the_real_snippet_preference_after_reopen() {
    use crate::snippet_window::ai_assist::Action;
    let scratch = crate::snippets::tests::Profile::new();
    let root = scratch.0.canonicalize().unwrap();
    let configuration = security_profiles();
    configuration
        .save_fixture_at(&root, &Configuration::default())
        .unwrap();
    let mut dialog = Dialog::new(
        Arc::new(configuration),
        root.clone(),
        Vec::new(),
        true,
        None,
    );
    let messages = search_and_press(
        &mut dialog,
        Setting::SecurityProfile,
        &[Named::Space, Named::End, Named::Enter],
    );
    for message in messages {
        let _ = dialog.update(message);
    }
    save_security_dialog(&mut dialog);
    let saved = Configuration::load_fixture_from(&root).unwrap();
    assert_eq!(
        saved.preferred_snippet_assistance_profile_id(Action::SecurityReview, None, |_| true),
        Some(12)
    );
    assert_eq!(
        saved.local.profile(local::Role::Text),
        Some(&ProfileRef::Numeric(11))
    );
    let mut reopened = Dialog::new(Arc::new(saved), root.clone(), Vec::new(), true, None);
    // Stay on General so an unrelated Local AI inventory refresh cannot block Save.
    assert_eq!(
        reopened.local_state.profile(local::Role::Security),
        Some(&ProfileRef::Numeric(12))
    );
    let messages = search_and_press(
        &mut reopened,
        Setting::ClearSecurityProfile,
        &[Named::Enter],
    );
    for message in messages {
        let _ = reopened.update(message);
    }
    save_security_dialog(&mut reopened);
    let saved = Configuration::load_fixture_from(&root).unwrap();
    assert_eq!(saved.local.profile(local::Role::Security), None);
    assert_eq!(
        saved.preferred_snippet_assistance_profile_id(Action::SecurityReview, None, |_| true),
        Some(11)
    );
}

#[test]
fn general_security_profile_readonly_search_cannot_change_the_shared_assignment() {
    let mut configuration = security_profiles();
    configuration
        .local
        .set_profile(local::Role::Security, Some(ProfileRef::Numeric(12)));
    let mut dialog = Dialog::new(
        Arc::new(configuration),
        PathBuf::new(),
        Vec::new(),
        false,
        None,
    );
    for setting in [Setting::SecurityProfile, Setting::ClearSecurityProfile] {
        let messages = search_and_press(&mut dialog, setting, &[Named::Space]);
        assert!(
            messages
                .iter()
                .all(|message| matches!(message, Message::Search(_)))
        );
        let _ = dialog.update(Message::Local(local::Message::Role(
            local::Role::Security,
            None,
        )));
        assert_eq!(
            dialog.local_state.profile(local::Role::Security),
            Some(&ProfileRef::Numeric(12))
        );
    }
}
