use std::cell::Cell;

use super::*;

fn profile_policy(create: bool, edit: bool) -> ClipboardPolicy {
    crate::clipboard_policy::parse(&format!(
        "[meta]\nschema-version = 1\n[[rule]]\n[rule.ai-profiles]\nallow-create = {create}\nallow-edit = {edit}\n"
    ))
    .unwrap()
}

fn saved_profiles() -> Configuration {
    Configuration::decode(
        br#"{"profiles":{"profiles":[
            {"id":1,"name":" Saved A ","model":"fixture","tavily_search":true},
            {"id":2,"name":"Saved B","model":"fixture"}
        ],"default":1,"last_id":2}}"#,
    )
    .unwrap()
}

#[test]
fn ai_manager_profile_policy_distinguishes_create_edit_delete_and_default() {
    let saved = saved_profiles();
    let create_only = profile_policy(true, false);
    let edit_only = profile_policy(false, true);
    let mut created = profiles::Editor::new(saved.profiles.clone());
    created.update(profiles::Message::Add, true);
    let id = created.selected().unwrap().id;
    created.update(profiles::Message::SetDefault(id), true);
    assert_eq!(created.catalog().default_profile().unwrap().id, id);
    let authorize = |editor: &profiles::Editor, policy: &ClipboardPolicy| {
        editor
            .catalog()
            .require_policy_changes(&saved.profiles, policy)
    };
    assert!(authorize(&created, &create_only).is_ok());
    assert!(authorize(&created, &edit_only).is_err());

    for events in [
        vec![profiles::Message::Edit(
            profiles::Field::Name,
            "Changed".into(),
        )],
        vec![profiles::Message::Delete, profiles::Message::ConfirmDelete],
        vec![
            profiles::Message::Select(2),
            profiles::Message::SetDefault(2),
        ],
    ] {
        let mut editor = profiles::Editor::new(saved.profiles.clone());
        for event in events {
            editor.update(event, true);
        }
        assert!(authorize(&editor, &create_only).is_err());
        assert!(authorize(&editor, &edit_only).is_ok());
    }

    let mut reordered = serde_json::to_value(&saved).unwrap();
    reordered["profiles"]["profiles"]
        .as_array_mut()
        .unwrap()
        .swap(0, 1);
    let reordered = Configuration::decode(&serde_json::to_vec(&reordered).unwrap()).unwrap();
    assert!(
        reordered
            .profiles
            .require_policy_changes(&saved.profiles, &create_only)
            .is_err()
    );

    created.update(
        profiles::Message::Edit(profiles::Field::Model, "fixture".into()),
        true,
    );
    let scratch = crate::snippets::tests::Profile::new();
    let root = scratch.0.canonicalize().unwrap();
    saved
        .save_with_policy(
            &root,
            &Configuration::default(),
            None,
            None,
            ClipboardPolicy::default,
        )
        .unwrap();
    let baseline = Configuration::load_from(&root).unwrap();
    let mut draft = baseline.clone();
    draft.profiles = created
        .catalog_for_save(Some(&baseline.profiles), true)
        .unwrap();
    draft
        .save_with_policy(&root, &baseline, None, None, || profile_policy(true, false))
        .unwrap();
    let committed = Configuration::load_from(&root).unwrap();
    assert_eq!(committed.profiles.default_profile().unwrap().id, id);
    assert!(
        baseline
            .profiles
            .entries()
            .all(|profile| committed.profiles.profile(profile.id) == Some(profile))
    );
}

#[test]
fn ai_manager_profile_policy_is_rechecked_at_first_write_and_replacement() {
    let scratch = crate::snippets::tests::Profile::new();
    let root = scratch.0.canonicalize().unwrap();
    let empty = Configuration::default();
    let initial = saved_profiles();
    let calls = Cell::new(0);
    let revoke_at_publication = |draft: &Configuration, baseline: &Configuration| {
        calls.set(0);
        let error = draft
            .save_with_policy(&root, baseline, None, None, || {
                let allow = calls.get() == 0;
                calls.set(calls.get() + 1);
                profile_policy(allow, allow)
            })
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(calls.get(), 2);
    };
    revoke_at_publication(&initial, &empty);
    assert!(!root.join(FILE_NAME).exists());
    initial
        .save_with_policy(&root, &empty, None, None, ClipboardPolicy::default)
        .unwrap();

    let saved = Configuration::load_from(&root).unwrap();
    let mut edited = saved.clone();
    assert!(edited.profiles.set_default(2));
    revoke_at_publication(&edited, &saved);
    assert!(Configuration::load_from(&root).unwrap() == saved);
}

#[test]
fn ai_manager_profile_policy_preserves_locked_profiles_during_other_saves() {
    let scratch = crate::snippets::tests::Profile::new();
    let root = scratch.0.canonicalize().unwrap();
    saved_profiles()
        .save_with_policy(
            &root,
            &Configuration::default(),
            None,
            None,
            ClipboardPolicy::default,
        )
        .unwrap();
    let saved = Configuration::load_from(&root).unwrap();
    let editor = profiles::Editor::new(saved.profiles.clone());
    let mut changed = saved.clone();
    changed.profiles = editor
        .catalog_for_save(Some(&saved.profiles), true)
        .unwrap();
    changed.general.timeout_minutes += 1;
    changed
        .save_with_policy(&root, &saved, None, None, || profile_policy(false, false))
        .unwrap();
    let current = Configuration::load_from(&root).unwrap();
    assert_eq!(current.profiles, saved.profiles);
    assert_eq!(
        current.general.timeout_minutes,
        changed.general.timeout_minutes
    );

    let mut clamped = current.clone();
    clamped.profiles = editor
        .catalog_for_save(Some(&current.profiles), false)
        .unwrap();
    let deny_web_search = || {
        crate::clipboard_policy::parse(
            "[meta]\nschema-version = 1\n[[rule]]\n[rule.ai-profiles]\nallow-edit = false\nallow-internet = false\n",
        )
        .unwrap()
    };
    assert!(
        clamped
            .save_with_policy(&root, &current, None, None, || profile_policy(false, false))
            .is_err()
    );
    clamped
        .save_with_policy(&root, &current, None, None, deny_web_search)
        .unwrap();
    let current = Configuration::load_from(&root).unwrap();
    let mut expected = saved.profiles.profile(1).unwrap().clone();
    expected.tavily_search = false;
    assert!(current.profiles.profile(1) == Some(&expected));
}
