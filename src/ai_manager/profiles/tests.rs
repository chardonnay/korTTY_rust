use super::*;

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
    editor.update(Message::SetDefault, true);
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
