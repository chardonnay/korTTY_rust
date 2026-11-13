use super::*;

#[test]
fn java_picture_in_picture_is_an_arrangement_and_preserves_the_selected_modules() {
    let layout = Layout {
        history: History::Timeline,
        status: Status::Powerline,
        arrangement: Arrangement::PictureInPicture,
        ..Layout::default()
    };
    let raw = serde_json::to_value(layout).unwrap();
    assert_eq!(raw["arrangement"], "d19");
    assert_eq!(serde_json::from_value::<Layout>(raw).unwrap(), layout);
}

#[test]
fn in_terminal_histories_preserve_java_ids_and_leave_other_roles_independent() {
    for (history, id) in [(History::InlineBlocks, "d03"), (History::Focus, "d17")] {
        let layout = Layout {
            history,
            status: Status::Metro,
            approval: Approval::Cursor,
            arrangement: Arrangement::PictureInPicture,
        };
        let raw = serde_json::to_value(layout).unwrap();
        assert_eq!(raw["history"], id);
        assert!(history.inline_style().is_some());
        assert_eq!(serde_json::from_value::<Layout>(raw).unwrap(), layout);
    }
}

#[test]
fn native_java_modules_round_trip_without_changing_legacy_role_ids() {
    assert_eq!(
        serde_json::to_string(&Layout::default()).unwrap(),
        r#"{"history":"transcript","status":"steps","approval":"below_activity","arrangement":"normal"}"#
    );
    assert_eq!(
        serde_json::to_string(&History::StepCards).unwrap(),
        r#""step_cards""#
    );
    assert_eq!(serde_json::to_string(&Status::Pill).unwrap(), r#""pill""#);
    assert_eq!(
        serde_json::to_string(&Arrangement::Floating).unwrap(),
        r#""floating""#
    );
    for (history, id) in [
        (History::Timeline, "d02"),
        (History::PlanExecution, "d06"),
        (History::CardStack, "d07"),
        (History::Tmux, "d09"),
        (History::Conversation, "d13"),
        (History::LogTable, "d16"),
    ] {
        assert_eq!(serde_json::to_value(history).unwrap(), id);
        for status in Status::ALL {
            let mut catalog = Catalog::default();
            let custom = catalog.duplicate("standard").unwrap();
            let layout = Layout {
                history,
                status,
                arrangement: Arrangement::Adaptive,
                approval: Approval::AboveActivity,
            };
            catalog.set_layout(&custom.id, layout);
            assert!(catalog.set_custom_default(&custom.id));
            let restored: Catalog =
                serde_json::from_slice(&serde_json::to_vec(&catalog).unwrap()).unwrap();
            assert_eq!(restored.resolve(Some(&custom.id), Preset::Classic), layout);
            assert_eq!(restored.default_id(Preset::Classic), custom.id);
        }
    }
}

#[test]
fn custom_agent_design_resolves_roles_and_never_reuses_deleted_ids() {
    let mut catalog = Catalog::default();
    let original = catalog.duplicate("floating").unwrap();
    assert!(catalog.rename(&original.id, "Ops cards"));
    assert!(!catalog.rename(&original.id, "Classic"));
    let layout = Layout {
        history: History::StepCards,
        approval: Approval::AboveActivity,
        ..Preset::Floating.layout()
    };
    catalog.set_layout(&original.id, layout);
    assert!(catalog.set_custom_default(&original.id));
    assert_eq!(
        catalog.resolve(Some("classic"), Preset::Standard),
        Preset::Classic.layout()
    );
    assert_eq!(catalog.resolve(Some("unknown"), Preset::Standard), layout);
    assert_eq!(
        catalog.connection_choices(Preset::Standard)[0].name,
        "Use default (Ops cards)"
    );

    let bytes = serde_json::to_vec(&catalog).unwrap();
    let mut catalog: Catalog = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(catalog.resolve(None, Preset::Standard), layout);
    catalog.remove(&original.id);
    assert_eq!(
        catalog.resolve(Some(&original.id), Preset::Standard),
        Layout::default()
    );
    assert_ne!(catalog.duplicate("standard").unwrap().id, original.id);
}

#[test]
fn unknown_custom_agent_modules_fall_back_by_role() {
    let catalog: Catalog = serde_json::from_str(
        r#"{
        "default_id":"custom-17",
        "designs":[{"id":"custom-17","name":"My design","layout":{
            "history":"future_history","status":"pill","approval":"future_approval",
            "arrangement":"floating"
        }}]
    }"#,
    )
    .unwrap();
    assert_eq!(catalog.last_id, 17);
    assert_eq!(
        catalog.resolve(None, Preset::Classic),
        Layout {
            status: Status::Pill,
            arrangement: Arrangement::Floating,
            ..Layout::default()
        }
    );
}

#[test]
fn custom_agent_design_catalog_bounds_and_stale_default_fallback() {
    let mut catalog = Catalog::default();
    for _ in 0..MAX_DESIGNS {
        catalog.duplicate("classic").unwrap();
    }
    assert!(!catalog.can_duplicate());
    assert!(catalog.duplicate("standard").is_none());
    let mut saved = serde_json::to_value(&catalog).unwrap();
    saved["default_id"] = serde_json::Value::String("custom-999".into());
    saved["designs"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "id": "custom-50", "name": "Future design", "layout": {}
        }));
    let mut catalog: Catalog = serde_json::from_value(saved).unwrap();
    assert_eq!(catalog.choices().len(), Preset::ALL.len() + MAX_DESIGNS);
    assert_eq!(catalog.default_id(Preset::Classic), "classic");
    catalog.remove("custom-1");
    assert_eq!(catalog.duplicate("standard").unwrap().id, "custom-51");
}
