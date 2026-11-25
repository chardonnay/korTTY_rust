use super::*;

fn profile() -> crate::snippets::tests::Profile {
    crate::snippets::tests::Profile::new()
}

#[test]
fn shared_defaults_use_existing_stores_and_preserve_unrelated_preferences() {
    let root = profile();
    let path = crate::profile::window_settings_path_at(&root.0);
    let initial = WindowSettings {
        snippet_alternative_count: Some(4),
        ..WindowSettings::default()
    };
    initial.save(&path).unwrap();
    let mut state = State::load(&root.0);
    assert_eq!(state.snippets().unwrap().alternatives, 4);
    state.set_instructions(true);
    state.set_history_limit("5".into());
    for i in 0..8 {
        history::remember(
            &root.0,
            format!("fixture prompt {i}"),
            crate::terminal_agent::Mode::Run,
            false,
        )
        .unwrap();
    }
    // Reload the history snapshot before reviewing its destructive limit change.
    state.history = Some(history::list(&root.0, false).unwrap());
    let mut concurrent = WindowSettings::read(&path).unwrap();
    concurrent.terminal_transparency = 17;
    concurrent.save(&path).unwrap();
    assert!(state.save(&root.0).is_none());
    let saved = WindowSettings::read(&path).unwrap();
    assert!(saved.snippet_ai_additional_instructions());
    assert_eq!(saved.snippet_alternative_count, Some(4));
    assert_eq!(saved.terminal_transparency, 17);
    let saved = history::list(&root.0, false).unwrap();
    assert_eq!(saved.limit, 5);
    assert_eq!(saved.entries.len(), 5);
    assert_eq!(saved.entries[0].goal, "fixture prompt 7");
    assert!(!state.dirty());
    let (snippets, history) = state.take_updates();
    assert!(snippets.unwrap().instructions);
    assert_eq!(history.unwrap().entries.len(), 5);
    let reopened = State::load(&root.0);
    assert_eq!(reopened.history_limit, "5");
    assert!(reopened.snippets().unwrap().instructions);
}

#[test]
fn shared_defaults_rebase_untouched_fields_but_reject_a_conflicting_edit() {
    let root = profile();
    let path = crate::profile::window_settings_path_at(&root.0);
    let mut state = State::load(&root.0);
    state.set_alternatives(6);
    let current = SnippetValues {
        instructions: true,
        alternatives: 4,
    };
    state.install_snippets(current);
    assert_eq!(
        state.snippets(),
        Some(SnippetValues {
            instructions: true,
            alternatives: 6
        })
    );
    let settings = WindowSettings {
        snippet_ai_additional_instructions: Some(true),
        snippet_alternative_count: Some(4),
        ..WindowSettings::default()
    };
    settings.save(&path).unwrap();
    assert!(
        state
            .save(&root.0)
            .unwrap()
            .contains("changed in another dialog")
    );
    assert_eq!(
        WindowSettings::read(&path)
            .unwrap()
            .snippet_alternative_count,
        Some(4)
    );
    assert!(state.dirty());
    assert!(state.take_updates().0.is_none());
}

#[test]
fn shared_defaults_history_conflict_does_not_discard_new_prompts_or_hide_partial_save() {
    let root = profile();
    let mut state = State::load(&root.0);
    state.set_alternatives(7);
    state.set_history_limit("5".into());
    history::remember(
        &root.0,
        "new prompt after dialog opened".into(),
        crate::terminal_agent::Mode::Run,
        false,
    )
    .unwrap();
    let error = state.save(&root.0).unwrap();
    assert!(error.contains("prompt limit was not saved"));
    let history = history::list(&root.0, false).unwrap();
    assert_eq!(history.limit, 20);
    assert_eq!(history.entries[0].goal, "new prompt after dialog opened");
    assert_eq!(
        WindowSettings::read(&crate::profile::window_settings_path_at(&root.0))
            .unwrap()
            .snippet_alternative_count,
        Some(7)
    );
    assert_eq!(state.take_updates().0.unwrap().alternatives, 7);
    assert!(state.dirty());
}

#[test]
fn shared_defaults_reject_invalid_limits_and_keep_corrupt_stores_unchanged() {
    let root = profile();
    let mut state = State::load(&root.0);
    for value in ["", "4", "101", "no"] {
        state.set_history_limit(value.into());
        assert!(!state.valid());
    }
    for value in ["5", "100"] {
        state.set_history_limit(value.into());
        assert!(state.valid());
    }
    let path = crate::profile::window_settings_path_at(&root.0);
    std::fs::write(&path, b"broken fixture").unwrap();
    let state = State::load(&root.0);
    assert!(state.snippets().is_none());
    assert!(state.problem.contains("Could not load snippet defaults"));
    assert_eq!(std::fs::read(path).unwrap(), b"broken fixture");
}
