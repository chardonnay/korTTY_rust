use super::*;

#[test]
fn model_prompt_preset_persists_and_legacy_profiles_keep_generic_requests() {
    use local_ai::PromptPreset;

    let mut profile: Profile = serde_json::from_value(serde_json::json!({
        "id": 1,
        "name": "Prompt family fixture",
        "model": "Qwen3-4B"
    }))
    .unwrap();
    let root = Path::new(".");
    let system = "Keep these bytes. \n";
    let transport = profile.transport_in(1, root, None).unwrap();
    assert_eq!(transport.prompt_preset(), PromptPreset::Generic);
    assert_eq!(transport.render_system_prompt(system), system);
    assert!(
        serde_json::to_value(&profile)
            .unwrap()
            .get("prompt_preset")
            .is_none()
    );

    for (preset, persisted, effective) in [
        (PromptPreset::Auto, "auto", PromptPreset::Qwen),
        (PromptPreset::DeepSeek, "deepseek", PromptPreset::DeepSeek),
        (PromptPreset::GptOss, "gpt-oss", PromptPreset::GptOss),
        (PromptPreset::Generic, "generic", PromptPreset::Generic),
    ] {
        profile.prompt_preset = Some(preset);
        let saved = serde_json::to_value(&profile).unwrap();
        assert_eq!(saved["prompt_preset"], persisted);
        let reopened: Profile = serde_json::from_value(saved).unwrap();
        let transport = reopened.transport_in(1, root, None).unwrap();
        assert_eq!(reopened.prompt_preset, Some(preset));
        assert_eq!(reopened.effective_prompt_preset(), effective);
        assert_eq!(transport.prompt_preset(), effective);
        let rendered = transport.render_system_prompt(system);
        assert!(rendered.starts_with(system));
        if let Some(guidance) = effective.guidance() {
            assert!(rendered.contains(guidance));
        } else {
            assert_eq!(rendered, system);
        }
    }
}

#[test]
fn new_ai_profile_selects_auto_and_preset_edits_require_an_editable_form() {
    use local_ai::PromptPreset;

    let mut editor = Editor::new(Catalog::default());
    editor.update(Message::Add, true);
    assert_eq!(
        editor.selected().unwrap().prompt_preset,
        Some(PromptPreset::Auto)
    );
    editor.update(Message::Edit(Field::Model, "mixtral-8x7b".into()), true);
    editor.update(Message::PromptPreset(PromptPreset::Generic), false);
    assert_eq!(
        editor.selected().unwrap().effective_prompt_preset(),
        PromptPreset::Mistral
    );
    editor.update(Message::PromptPreset(PromptPreset::Generic), true);
    let saved = editor.validated_catalog().unwrap();
    assert_eq!(
        saved.default_profile().unwrap().prompt_preset,
        Some(PromptPreset::Generic)
    );
}

#[test]
fn saved_forecast_tokenizer_survives_reopening_and_reaches_the_request() {
    let profile: Profile = serde_json::from_value(serde_json::json!({
        "id": 1,
        "name": "Token count",
        "model": "fixture",
        "tokenizer": "o200k_base"
    }))
    .unwrap();
    let saved = serde_json::to_value(&profile).unwrap();
    assert_eq!(saved["tokenizer"], "o200k_base");
    let reopened: Profile = serde_json::from_value(saved).unwrap();
    let transport = reopened
        .transport_in(1, std::path::Path::new("."), None)
        .unwrap();
    assert_eq!(transport.count_input(["お誕生日おめでとう"]), 20);
    assert_eq!(Profile::default().tokenizer, local_ai::Tokenizer::Estimate);
}

#[test]
fn saved_profile_references_preserve_legacy_numbers_and_exact_policy_identifiers() {
    let configured = |model| {
        crate::clipboard_policy::parse(&format!(
            "[meta]\nschema-version = 1\n[[ai-profile]]\nid = 'policy-Example'\nname = 'Example'\nprovider = 'lm-studio'\nendpoint = 'http://127.0.0.1:1234'\nmodel = '{model}'\n"
        ))
        .unwrap()
    };
    let policy = configured("reviewed");
    let mut catalog = Catalog::default();
    catalog.project_policy_profiles(&policy).unwrap();
    let handle = catalog.default_profile().unwrap().id;
    assert!(handle > ProfileId::from(u64::MAX));
    assert!(serde_json::to_vec(catalog.profile(handle).unwrap()).is_err());
    let named = catalog.profile_reference(handle).unwrap();
    assert_eq!(named, ProfileRef::Policy("policy-Example".into()));
    let serialized = serde_json::to_string(&named).unwrap();
    assert_eq!(serialized, "\"policy-Example\"");
    assert_eq!(
        serde_json::from_str::<ProfileRef>(&serialized).unwrap(),
        named
    );
    assert_eq!(catalog.resolve_profile_reference(&named), Some(handle));
    assert!(
        catalog
            .require_current_profile_reference(&named, &policy)
            .is_ok()
    );
    assert!(
        catalog
            .require_current_profile_reference(&named, &configured("replacement"))
            .is_err()
    );
    assert_eq!(
        catalog.resolve_profile_reference(&ProfileRef::Policy("policy-example".into())),
        None
    );
    assert_eq!(Catalog::default().resolve_profile_reference(&named), None);
    assert!(Catalog::default().is_policy_profile_reference(&named));

    let alias = catalog.legacy_policy_alias(handle).unwrap();
    let serialized = alias.to_string();
    let numeric: ProfileRef = serde_json::from_str(&serialized).unwrap();
    assert_eq!(numeric, ProfileRef::Numeric(alias));
    assert_eq!(serde_json::to_string(&numeric).unwrap(), serialized);
    assert_eq!(catalog.resolve_profile_reference(&numeric), Some(handle));
    assert!(catalog.is_policy_profile_reference(&numeric));

    let mut legacy = Catalog {
        profiles: vec![Profile {
            id: ProfileId::from(alias),
            ..Profile::default()
        }],
        default: Some(alias),
        last_id: u64::MAX,
        ..Catalog::default()
    };
    legacy.project_policy_profiles(&policy).unwrap();
    assert_eq!(
        legacy.profile_reference(ProfileId::from(alias)),
        Some(numeric.clone())
    );
    assert_eq!(
        legacy.resolve_profile_reference(&numeric),
        Some(ProfileId::from(alias))
    );
    assert_eq!(legacy.resolve_profile_reference(&named), Some(handle));
    assert_eq!(legacy.legacy_policy_alias(handle), None);
    assert_eq!(
        legacy.resolve_profile_reference(&ProfileRef::Numeric(alias ^ 1)),
        None
    );
    assert!(!legacy.is_policy_profile_reference(&numeric));
    let mut exhausted = Editor::new(legacy.clone());
    exhausted.update(Message::Add, true);
    assert_eq!(exhausted.catalog.profiles.len(), 1);
    assert_eq!(
        exhausted.catalog.resolve_profile_reference(&named),
        Some(handle)
    );

    legacy.last_id = alias;
    let mut available = Editor::new(legacy);
    available.update(Message::Add, true);
    assert_eq!(available.catalog.last_id, alias + 1);
    assert_eq!(available.selected, Some(ProfileId::from(alias + 1)));
    assert_eq!(
        available.catalog.resolve_profile_reference(&named),
        Some(handle)
    );
    for invalid in [
        "\"Example\"",
        "\"policy-\"",
        "\" policy-Example\"",
        "\"23\"",
    ] {
        assert!(serde_json::from_str::<ProfileRef>(invalid).is_err());
    }
}

#[test]
fn selecting_the_effective_default_can_replace_a_missing_policy_default() {
    let catalog = Catalog {
        profiles: vec![Profile {
            id: 7,
            ..Profile::default()
        }],
        default: Some(7),
        last_id: 7,
        policy_default: Some("policy-Unavailable".into()),
        ..Catalog::default()
    };
    assert_eq!(catalog.default_profile().unwrap().id, 7);
    assert!(!catalog.is_marked_default(7));
    let mut editor = Editor::new(catalog);
    assert!(editor.can_set_default(7));
    editor.update(Message::SetDefault(7), true);
    assert_eq!(editor.catalog.policy_default, None);
    assert!(editor.catalog.is_marked_default(7));
    assert!(!editor.can_set_default(7));
}

#[test]
fn profile_actions_are_reachable_above_both_columns() {
    use iced::advanced::{Layout, Shell, clipboard, layout, widget::Tree};
    use iced::{Event, Font, Renderer, Size, mouse};

    let mut editor = Editor::default();
    editor.update(Message::Add, true);
    let reveal = crate::ai_manager::search::Reveal::default();
    let renderer = Renderer::new(Font::DEFAULT, 16.0.into());
    for (size, create) in [
        (Size::new(480.0, 280.0), true),
        (Size::new(960.0, 580.0), true),
        (Size::new(480.0, 280.0), false),
    ] {
        let mut view = editor.view(true, create, create, None, &reveal);
        let mut tree = Tree::new(view.as_widget());
        let node = view.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, size),
        );
        let layout = Layout::new(&node);
        let mut children = layout.children();
        let actions = children.next().unwrap();
        let content = children.next().unwrap();
        assert_eq!(actions.children().count(), if create { 3 } else { 4 });
        assert_eq!(content.children().count(), 2);
        let wizard = actions.children().next().unwrap().bounds();
        for (index, action) in actions.children().take(3).enumerate() {
            let action = action.bounds();
            assert!(action.width > 0.0 && action.height > 0.0);
            assert_eq!(action.y, wizard.y);
            assert!(action.y + action.height <= content.bounds().y);
            assert!(action.x + action.width <= size.width);

            let mut messages = Vec::new();
            let cursor = mouse::Cursor::Available(action.center());
            for event in [
                mouse::Event::ButtonPressed(mouse::Button::Left),
                mouse::Event::ButtonReleased(mouse::Button::Left),
            ] {
                let mut shell = Shell::new(&mut messages);
                view.as_widget_mut().update(
                    &mut tree,
                    &Event::Mouse(event),
                    layout,
                    cursor,
                    &renderer,
                    &mut clipboard::Null,
                    &mut shell,
                    &layout.bounds(),
                );
            }
            assert!(match (create, index) {
                (_, 2) => matches!(messages.as_slice(), [Message::Refresh]),
                (true, 0) => matches!(messages.as_slice(), [Message::Wizard(WizardAction::Open)]),
                (true, 1) => matches!(messages.as_slice(), [Message::Add]),
                (false, _) => messages.is_empty(),
                _ => false,
            });
        }
    }
}

#[test]
fn profiles_keep_stable_ids_and_replace_deleted_default() -> Result<(), String> {
    let mut editor = Editor::default();
    editor.update(Message::Add, false);
    assert!(editor.selected().is_none());
    editor.update(Message::Add, true);
    let first = editor.selected().ok_or("missing first profile")?.id;
    editor.update(Message::Add, true);
    let second = editor.selected().ok_or("missing second profile")?.id;
    assert_ne!(first, second);
    assert_eq!(
        editor.catalog().default_profile().map(|profile| profile.id),
        Some(first)
    );
    editor.update(Message::SetDefault(second), true);
    editor.update(Message::Delete, true);
    editor.update(Message::ConfirmDelete, true);
    assert_eq!(
        editor.catalog().default_profile().map(|profile| profile.id),
        Some(first)
    );
    editor.update(Message::Add, true);
    assert!(editor.selected().is_some_and(|profile| profile.id > second));
    editor.validated_catalog()?;
    Ok(())
}

#[test]
fn editing_endpoint_clears_trust_and_discovered_models() -> Result<(), String> {
    let mut editor = Editor::default();
    editor.update(Message::Add, true);
    let profile = editor.selected().ok_or("missing test profile")?;
    let id = profile.id;
    let endpoint = profile.endpoint.clone();
    editor.update(Message::TrustLocal(true), true);
    editor.update(
        Message::Edit(Field::Credential, "KORTTY_TEST_KEY".into()),
        true,
    );
    editor.set_models(id, &endpoint, vec!["example-model".into()]);
    assert_eq!(editor.models, ["example-model"]);
    editor.update(
        Message::Edit(Field::Endpoint, "127.0.0.1:1234".into()),
        true,
    );
    editor.set_models(id, &endpoint, vec!["stale-model".into()]);
    let profile = editor.selected().ok_or("missing edited profile")?;
    assert!(!profile.trusted_local_endpoint);
    assert_eq!(profile.credential()?, None);
    assert!(editor.models.is_empty());
    Ok(())
}

#[test]
fn invalid_numeric_edits_block_save_until_repaired() -> Result<(), String> {
    let mut editor = Editor::default();
    editor.update(Message::Add, true);
    editor.update(Message::Override(Number::OutputTokens, true), true);
    editor.update(Message::Number(Number::OutputTokens, "64".into()), true);
    assert!(editor.validated_catalog().is_err());
    assert!(editor.draft_dirty());
    editor.update(Message::Number(Number::OutputTokens, "1024".into()), true);
    let catalog = editor.validated_catalog()?;
    assert_eq!(
        catalog
            .default_profile()
            .and_then(|profile| profile.max_output_tokens),
        Some(1024)
    );
    Ok(())
}

#[test]
fn profile_wizard_discards_canceled_candidate_and_keeps_discovered_model_on_finish()
-> Result<(), String> {
    let mut editor = Editor::default();
    editor.update(Message::Add, true);
    let first = editor.selected().ok_or("missing existing profile")?.id;
    editor.update(Message::Wizard(wizard::Action::Open), true);
    let canceled = editor.selected().ok_or("missing wizard candidate")?.id;
    editor.update(Message::Wizard(wizard::Action::Cancel), true);
    assert_eq!(editor.selected().map(|profile| profile.id), Some(first));
    assert!(editor.catalog().profile(canceled).is_none());

    editor.update(Message::Wizard(wizard::Action::Open), true);
    editor.update(Message::Wizard(wizard::Action::Next), true);
    editor.update(Message::ModelMode(ModelMode::Auto), true);
    let profile = editor.selected().ok_or("missing second wizard candidate")?;
    let id = profile.id;
    let endpoint = profile.endpoint.clone();
    assert_ne!(id, canceled);
    editor.set_models(canceled, &endpoint, vec!["stale-model".into()]);
    assert!(
        editor
            .selected()
            .is_some_and(|profile| profile.model.is_empty())
    );
    editor.set_models(id, &endpoint, vec!["detected-model".into()]);
    editor.update(Message::Wizard(wizard::Action::Next), true);
    editor.update(Message::Wizard(wizard::Action::Next), true);
    editor.update(Message::Wizard(wizard::Action::Finish), true);

    let catalog = editor.validated_catalog()?;
    let profile = catalog.profile(id).ok_or("wizard did not keep profile")?;
    assert_eq!(profile.model, "detected-model");
    assert_eq!(profile.model_mode, ModelMode::Auto);
    assert!(profile.transport(1).is_ok());
    Ok(())
}

#[test]
fn saved_profile_validation_rejects_remote_endpoints_and_untrusted_key_references() {
    let mut profile = Profile {
        id: 1,
        name: "Local model".into(),
        model: "example-model".into(),
        credential_env: "KORTTY_TEST_KEY".into(),
        ..Profile::default()
    };
    assert!(profile.validate().is_err());
    profile.trusted_local_endpoint = true;
    assert!(profile.validate().is_ok());
    profile.endpoint = "https://ai.example.invalid/v1".into();
    assert!(profile.validate().is_err());
    assert!(profile.transport(1).is_err());
}

#[test]
fn managed_profile_selects_registry_id_without_retaining_external_route() -> Result<(), String> {
    const MODEL_ID: &str = "0123456789abcdef0123456789abcdef";
    let legacy: Profile = serde_json::from_str(
        r#"{
        "id": 1, "name": "Existing local model", "endpoint": "127.0.0.1:9876",
        "model": "existing", "credential_env": "KORTTY_TEST_KEY", "trusted_local_endpoint": true
    }"#,
    )
    .map_err(|error| error.to_string())?;
    assert_eq!(legacy.connection_mode, ConnectionMode::External);
    assert!(legacy.transport(1).is_ok());
    assert!(
        serde_json::to_value(&legacy)
            .unwrap()
            .get("connection_mode")
            .is_none()
    );

    let mut editor = Editor::new(Catalog {
        profiles: vec![legacy],
        default: Some(1),
        last_id: 1,
        ..Catalog::default()
    });
    editor.update(Message::ConnectionMode(ConnectionMode::Managed), true);
    assert!(editor.validated_catalog().is_err());
    editor.update(Message::ManagedModel(MODEL_ID.into()), true);
    assert!(editor.validated_catalog().is_err());
    editor.update(
        Message::ManagedModelsLoaded(
            PolicyModelRuntime::Gguf,
            ManagedModels {
                choices: vec![ManagedChoice {
                    id: MODEL_ID.into(),
                    name: "GGUF fixture".into(),
                    vision: false,
                }],
                ..ManagedModels::default()
            },
        ),
        true,
    );
    editor.update(Message::ManagedModel(MODEL_ID.into()), true);
    editor.update(Message::TrustLocal(true), true);
    editor.update(Message::ModelMode(ModelMode::Auto), true);
    editor.update(Message::Vision(Vision::Enabled), true);
    editor.update(
        Message::Edit(Field::Endpoint, "127.0.0.1:1234".into()),
        true,
    );
    editor.update(
        Message::Edit(Field::Credential, "KORTTY_TEST_KEY".into()),
        true,
    );
    editor.set_models(1, "", vec!["stale external response".into()]);
    assert!(editor.models.is_empty());

    let saved = editor.validated_catalog()?;
    let serialized = serde_json::to_vec(&saved).map_err(|error| error.to_string())?;
    let restored: Catalog =
        serde_json::from_slice(&serialized).map_err(|error| error.to_string())?;
    restored.validate()?;
    let managed = restored
        .default_profile()
        .ok_or("missing managed profile")?;
    assert!(managed.is_managed());
    assert_eq!(managed.model, MODEL_ID);
    assert_eq!(managed.destination()?, MANAGED_DESTINATION);
    assert!(managed.endpoint.is_empty());
    assert!(!managed.trusted_local_endpoint);
    assert_eq!(managed.credential()?, None);
    assert_eq!(managed.vision, Vision::Disabled);
    let root = std::env::current_dir().map_err(|error| error.to_string())?;
    assert!(managed.transport_in(1, &root, None).is_ok());

    let mut invalid = managed.clone();
    invalid.credential_env = "KORTTY_TEST_KEY".into();
    assert!(invalid.transport_in(1, &root, None).is_err());
    Ok(())
}

#[test]
fn policy_managed_profile_alias_requires_a_choice_and_survives_refresh() {
    let alias = "policy-office-model";
    let mut editor = Editor::new(Catalog {
        profiles: vec![Profile {
            id: 1,
            name: "Administrative model".into(),
            ..Profile::default()
        }],
        default: Some(1),
        last_id: 1,
        ..Catalog::default()
    });
    editor.update(Message::ConnectionMode(ConnectionMode::Managed), true);
    editor.update(Message::ManagedModel(alias.into()), true);
    assert!(editor.validated_catalog().is_err());
    let administrative = ManagedChoice {
        id: alias.into(),
        name: "Office model".into(),
        vision: false,
    };
    assert_eq!(
        administrative.to_string(),
        "Office model (Managed by policy)"
    );
    editor.update(
        Message::ManagedModelsLoaded(
            PolicyModelRuntime::Mlx,
            ManagedModels {
                choices: vec![administrative.clone()],
                ..ManagedModels::default()
            },
        ),
        true,
    );
    editor.update(Message::ManagedModel(alias.into()), true);
    assert!(editor.validated_catalog().is_err());
    editor.update(
        Message::ManagedModelsLoaded(
            PolicyModelRuntime::Gguf,
            ManagedModels {
                choices: vec![administrative],
                policy_only: true,
                ..ManagedModels::default()
            },
        ),
        true,
    );
    editor.update(Message::ManagedModel(alias.into()), true);
    editor.update(Message::Vision(Vision::Enabled), true);
    let saved = editor.validated_catalog().unwrap();
    let decoded: Catalog = serde_json::from_value(serde_json::to_value(saved).unwrap()).unwrap();
    assert!(decoded.validate().is_ok());
    assert_eq!(decoded.default_profile().unwrap().vision, Vision::Disabled);
    editor.update(
        Message::ManagedModelsLoaded(PolicyModelRuntime::Gguf, ManagedModels::default()),
        true,
    );
    let selected = editor.selected().unwrap();
    assert_eq!(selected.model, alias);
    assert!(editor.managed_model_name(selected).is_none());
    assert!(editor.validated_catalog().is_ok());
}

#[test]
fn managed_vision_follows_the_registered_projector_choice() {
    let mut editor = Editor::new(Catalog {
        profiles: vec![Profile {
            id: 1,
            name: "Local fixture".into(),
            ..Profile::default()
        }],
        default: Some(1),
        last_id: 1,
        ..Catalog::default()
    });
    let vision_id = "a".repeat(32);
    let text_id = "b".repeat(32);
    editor.managed_models.choices = vec![
        ManagedChoice {
            id: vision_id.clone(),
            name: "Vision".into(),
            vision: true,
        },
        ManagedChoice {
            id: text_id.clone(),
            name: "Text".into(),
            vision: false,
        },
    ];
    editor.update(Message::ConnectionMode(ConnectionMode::Managed), true);
    editor.update(Message::ManagedModel(vision_id), true);
    editor.update(Message::Vision(Vision::Enabled), true);
    assert_eq!(
        editor
            .validated_catalog()
            .unwrap()
            .default_profile()
            .unwrap()
            .vision,
        Vision::Enabled
    );
    editor.update(Message::ManagedModel(text_id), true);
    editor.update(Message::Vision(Vision::Enabled), true);
    assert_eq!(
        editor
            .validated_catalog()
            .unwrap()
            .default_profile()
            .unwrap()
            .vision,
        Vision::Disabled
    );
}

#[test]
fn lm_studio_native_profile_preserves_mode_and_uses_the_native_destination() {
    let profile = Profile {
        id: 1,
        name: "Native LM Studio".into(),
        connection_mode: ConnectionMode::LmStudio,
        endpoint: "http://127.0.0.1:1234/api/v1/chat".into(),
        model: "fixture".into(),
        ..Profile::default()
    };
    assert!(profile.validate().is_ok());
    assert_eq!(
        profile.destination().unwrap(),
        "http://127.0.0.1:1234/api/v1/chat"
    );
    let restored: Profile = serde_json::from_slice(&serde_json::to_vec(&profile).unwrap()).unwrap();
    assert_eq!(restored.connection_mode, ConnectionMode::LmStudio);
    assert!(restored.transport(1).is_ok());
}

#[test]
fn remote_profile_requires_separate_trust_and_endpoint_edits_clear_credential_reference() {
    let profile = Profile {
        id: 1,
        name: "Remote model".into(),
        connection_mode: ConnectionMode::Anthropic,
        endpoint: "https://api.anthropic.com/v1".into(),
        model: "fixture".into(),
        ..Profile::default()
    };
    assert!(profile.transport(1).is_err());
    let mut editor = Editor::new(Catalog {
        profiles: vec![profile],
        default: Some(1),
        last_id: 1,
        ..Catalog::default()
    });
    editor.update(Message::TrustLocal(true), true);
    assert!(editor.validated_catalog().is_err());
    editor.update(Message::TrustRemote(true), true);
    editor.update(
        Message::Edit(Field::Credential, "FIXTURE_API_KEY".into()),
        true,
    );
    let trusted = editor.validated_catalog().unwrap();
    let trusted = trusted.default_profile().unwrap();
    assert!(trusted.transport(1).is_ok());
    assert_eq!(
        trusted.destination().unwrap(),
        "https://api.anthropic.com/v1/messages"
    );
    assert_eq!(
        trusted.credential().unwrap().as_deref(),
        Some("FIXTURE_API_KEY")
    );
    editor.update(
        Message::Edit(Field::Endpoint, "https://different.example.test/v1".into()),
        true,
    );
    let changed = editor.selected().unwrap();
    assert!(changed.credential_env.is_empty());
    assert!(!changed.trusted_remote_endpoint);
    assert!(changed.transport(1).is_err());
}

#[test]
fn minimax_https_preset_replaces_destination_and_requires_fresh_trust() {
    let profile = Profile {
        id: 1,
        name: "Remote model".into(),
        connection_mode: ConnectionMode::RemoteOpenAi,
        endpoint: "https://old.example.test/v1".into(),
        model: "old-model".into(),
        trusted_remote_endpoint: true,
        credential_env: "OLD_API_KEY".into(),
        ..Profile::default()
    };
    let mut editor = Editor::new(Catalog {
        profiles: vec![profile],
        default: Some(1),
        last_id: 1,
        ..Catalog::default()
    });
    editor.update(Message::CloudProvider(cloud::Provider::MiniMax), false);
    assert_eq!(
        editor.selected().unwrap().endpoint,
        "https://old.example.test/v1"
    );
    editor.update(Message::CloudProvider(cloud::Provider::MiniMax), true);
    let selected = editor.selected().unwrap();
    assert_eq!(
        selected.destination().unwrap(),
        "https://api.minimax.io/v1/chat/completions"
    );
    assert!(!selected.trusted_remote_endpoint);
    assert!(selected.credential_env.is_empty() && selected.model.is_empty());
    assert!(selected.transport(1).is_err());

    editor.update(Message::TrustRemote(true), true);
    editor.update(
        Message::Edit(Field::Credential, "MINIMAX_API_KEY".into()),
        true,
    );
    editor.update(Message::Edit(Field::Model, "MiniMax-M3".into()), true);
    let saved = editor.validated_catalog().unwrap();
    let saved = saved.default_profile().unwrap();
    assert_eq!(
        saved.credential().unwrap().as_deref(),
        Some("MINIMAX_API_KEY")
    );
    assert!(saved.transport(1).is_ok());
}

#[test]
fn cloud_presets_offer_offline_models_without_granting_endpoint_trust() {
    use cloud::Provider;

    let mut editor = Editor::default();
    editor.update(Message::Wizard(WizardAction::Open), true);
    for provider in Provider::ALL {
        editor.update(Message::CloudProvider(provider), true);
        let profile = editor.selected().unwrap();
        assert!(provider.matches_endpoint(profile));
        assert_eq!(profile.connection_mode, provider.mode());
        assert!(profile.model.is_empty() && profile.credential_env.is_empty());
        assert!(!profile.trusted_remote_endpoint && !profile.reviewed_agent_https);
        assert!(profile.validate_connection().is_err());
        assert!(!cloud::model_choices(profile, &[]).is_empty());
    }

    editor.update(Message::CloudProvider(Provider::Gemini), true);
    let profile = editor.selected().unwrap();
    assert_eq!(
        profile.destination().unwrap(),
        "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions"
    );
    let discovered = vec!["current-model".to_owned(), "gemini-2.0-flash".to_owned()];
    assert_eq!(
        cloud::model_choices(profile, &discovered),
        ["current-model", "gemini-2.0-flash", "gemini-1.5-pro"]
    );
    editor.update(Message::TrustRemote(true), true);
    editor.update(
        Message::Edit(Field::Credential, "FIXTURE_GEMINI_KEY".into()),
        true,
    );
    editor.update(Message::CloudProvider(Provider::Gemini), true);
    assert!(editor.selected().unwrap().trusted_remote_endpoint);
    editor.update(Message::CloudProvider(Provider::Anthropic), true);
    assert_eq!(
        editor.selected().unwrap().destination().unwrap(),
        "https://api.anthropic.com/v1/messages"
    );
    assert!(!editor.selected().unwrap().trusted_remote_endpoint);
    assert!(editor.selected().unwrap().credential_env.is_empty());
    editor.update(
        Message::Edit(
            Field::Endpoint,
            "https://api.anthropic.com.example.org/v1".into(),
        ),
        true,
    );
    assert!(cloud::model_choices(editor.selected().unwrap(), &[]).is_empty());
}
