use super::*;
use crate::ai_manager::{Configuration, backup};
use crate::clipboard_policy::ClipboardPolicy;

#[test]
fn journal_reevaluation_prefers_current_native_or_imported_previous_profile() {
    let _exclusive = exclusive_run();
    let (endpoint, _) = listener();
    let address = endpoint.local_addr().unwrap();
    let (scratch, mut workspace, capture, source) = seeded(address);
    let root = scratch.0.canonicalize().unwrap();
    let xml = format!(
        r#"<globalSettings><aiProfiles>
            <profile><id>previous-java-profile</id><name>Previous profile</name>
                <model>previous-model</model><apiUrl>http://{address}/v1/chat/completions</apiUrl></profile>
            <profile><id>configured-java-profile</id><name>Configured profile</name>
                <model>configured-model</model><apiUrl>http://{address}/v1/chat/completions</apiUrl></profile>
        </aiProfiles></globalSettings>"#
    );
    let (portable, _) = backup::portable_java_profiles(xml.as_bytes()).unwrap();
    let catalog = backup::merge_java_profiles(None, &portable, &ClipboardPolicy::default())
        .unwrap()
        .unwrap();
    let mut ready: serde_json::Value = serde_json::from_slice(&catalog).unwrap();
    for profile in ready["profiles"]["profiles"].as_array_mut().unwrap() {
        profile["restore_review"] = json!(false);
        profile["trusted_local_endpoint"] = json!(true);
    }
    let profiles = ready["profiles"]["profiles"].as_array().unwrap();
    let id_for_model = |model: &str| {
        profiles
            .iter()
            .find(|profile| profile["model"] == model)
            .unwrap()["id"]
            .as_u64()
            .unwrap()
    };
    let previous = id_for_model("previous-model");
    let configured = id_for_model("configured-model");
    ready["local"]["roles"] = json!([null, null, null, configured]);
    let configuration: Configuration = serde_json::from_value(ready).unwrap();
    configuration
        .save_fixture_at(&root, &Configuration::default())
        .unwrap();
    workspace.set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());
    finish_capture(&capture);

    let document = source.directory().join("journal.xml");
    let base = std::fs::read_to_string(&document).unwrap();
    let manager = show_manager(&mut workspace);
    for (index, (metadata, expected)) in [
        (
            format!("<nativeAiProfileRef>{previous}</nativeAiProfileRef>"),
            previous,
        ),
        (
            "<aiProfileId>previous-java-profile</aiProfileId>".into(),
            previous,
        ),
        (
            format!("<nativeAiProfileRef>{configured}</nativeAiProfileRef><aiProfileId>previous-java-profile</aiProfileId>"),
            previous,
        ),
        (
            "<aiProfileId>removed-java-profile</aiProfileId>".into(),
            configured,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        std::fs::write(
            &document,
            base.replacen("</meta>", &format!("{metadata}</meta>"), 1),
        )
        .unwrap();
        if index > 0 {
            let _ = workspace.refresh_journal_manager(manager, false);
        }
        review(&mut workspace, manager, &source);
        let Some(Review::Run { selected, .. }) = workspace.journal_summaries.reviews.get(&manager)
        else {
            panic!("expected a journal re-evaluation review");
        };
        assert_eq!(
            selected.profile.as_ref().unwrap().id,
            ProfileRef::Numeric(expected)
        );
        let _ = summary(&mut workspace, manager, Event::Dismiss);
    }
    assert_eq!(endpoint.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
}

#[test]
fn imported_java_journal_role_choices_require_separate_settings_adoption() {
    let _exclusive = exclusive_run();
    let (endpoint, _) = listener();
    let address = endpoint.local_addr().unwrap();
    let (scratch, mut workspace, capture, _) = seeded(address);
    let root = scratch.0.canonicalize().unwrap();
    let xml = format!(
        r#"<globalSettings>
        <sessionJournalAiProfileId>source-recording</sessionJournalAiProfileId>
        <automationJournalAiProfileId>source-automation</automationJournalAiProfileId>
        <aiProfiles>
            <profile><id>source-recording</id><name>Java recording</name>
                <model>recording-model</model><apiUrl>https://{address}/recording/v1</apiUrl></profile>
            <profile><id>source-automation</id><name>Java automation</name>
                <model>automation-model</model><apiUrl>https://{address}/automation/v1</apiUrl></profile>
        </aiProfiles>
    </globalSettings>"#
    );
    let (portable, report) = backup::portable_java_profiles(xml.as_bytes()).unwrap();
    assert_eq!(report.https_imported, 2);
    let catalog = backup::merge_java_profiles(None, &portable, &ClipboardPolicy::default())
        .unwrap()
        .unwrap();
    let draft: Configuration = serde_json::from_slice(&catalog).unwrap();
    draft
        .save_fixture_at(&root, &Configuration::default())
        .unwrap();
    let imported = Configuration::load_fixture_from(&root).unwrap();
    workspace.set_test_ai_configuration(imported.clone());
    let manager = show_manager(&mut workspace);
    let _ = summary(&mut workspace, manager, Event::Settings);
    let _ = summary(&mut workspace, manager, Event::UseImportedProfile(false));
    assert_eq!(
        workspace.journal_summaries.status.as_deref(),
        Some(IMPORT_CHANGED)
    );
    assert!(matches!(workspace.journal_summaries.reviews.get(&manager),
        Some(Review::Settings { selected, automation: AutomationChoice::Inherit, .. }) if selected.profile.is_none()));
    let _ = summary(&mut workspace, manager, Event::Dismiss);

    let mut ready: serde_json::Value = serde_json::from_slice(&catalog).unwrap();
    for profile in ready["profiles"]["profiles"].as_array_mut().unwrap() {
        profile["restore_review"] = json!(false);
        profile["trusted_remote_endpoint"] = json!(true);
    }
    let ready: Configuration = serde_json::from_value(ready.clone()).unwrap();
    ready.save_fixture_at(&root, &imported).unwrap();
    workspace.set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());
    let _ = summary(&mut workspace, manager, Event::Settings);
    let _ = summary(&mut workspace, manager, Event::UseImportedProfile(false));
    let _ = summary(&mut workspace, manager, Event::Enable(true));
    let disk = Queue::load(&root).unwrap();
    assert!(disk.preferences().reviewed_grant().is_none());
    assert!(disk.preferences().automation_grant().is_none());
    let _ = summary(&mut workspace, manager, Event::SaveSettings);
    let disk = Queue::load(&root).unwrap();
    let preferences = disk.preferences();
    let recording = preferences.reviewed_grant().unwrap();
    assert_eq!(recording.profile.model, "recording-model");
    assert!(preferences.automation_grant().is_none());
    assert_eq!(
        preferences.automation_profile,
        Some(AutomationProfile::Local)
    );

    let _ = summary(&mut workspace, manager, Event::Settings);
    let _ = summary(&mut workspace, manager, Event::UseImportedProfile(true));
    assert!(
        Queue::load(&root)
            .unwrap()
            .preferences()
            .automation_grant()
            .is_none()
    );
    let _ = summary(&mut workspace, manager, Event::SaveSettings);
    let disk = Queue::load(&root).unwrap();
    let preferences = disk.preferences();
    assert_eq!(preferences.reviewed_grant(), Some(recording));
    assert_eq!(
        preferences.automation_grant().unwrap().profile.model,
        "automation-model"
    );
    assert_eq!(endpoint.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    finish_capture(&capture);
}

#[test]
fn imported_java_loopback_recording_profile_runs_the_reviewed_model() {
    let _exclusive = exclusive_run();
    let (endpoint, _) = listener();
    let address = endpoint.local_addr().unwrap();
    let (scratch, mut workspace, capture, source) = seeded(address);
    let root = scratch.0.canonicalize().unwrap();
    let xml = format!(
        r#"<globalSettings>
            <sessionJournalAiProfileId>local-recording</sessionJournalAiProfileId>
            <aiProfiles><profile><id>local-recording</id><name>Java local recording</name>
                <model>reviewed-recording-model</model><apiUrl>http://{address}/v1/chat/completions</apiUrl>
            </profile></aiProfiles>
        </globalSettings>"#
    );
    let (portable, _) = backup::portable_java_profiles(xml.as_bytes()).unwrap();
    let catalog = backup::merge_java_profiles(None, &portable, &ClipboardPolicy::default())
        .unwrap()
        .unwrap();
    let draft: Configuration = serde_json::from_slice(&catalog).unwrap();
    draft
        .save_fixture_at(&root, &Configuration::default())
        .unwrap();
    let imported = Configuration::load_fixture_from(&root).unwrap();
    assert!(imported.terminal_chat_transport(1).is_err());
    workspace.set_test_ai_configuration(imported.clone());
    let manager = show_manager(&mut workspace);
    let _ = summary(&mut workspace, manager, Event::Settings);
    let _ = summary(&mut workspace, manager, Event::UseImportedProfile(false));
    assert_eq!(
        workspace.journal_summaries.status.as_deref(),
        Some(IMPORT_CHANGED)
    );
    let _ = summary(&mut workspace, manager, Event::Dismiss);

    let mut ready: serde_json::Value = serde_json::from_slice(&catalog).unwrap();
    ready["profiles"]["profiles"][0]["restore_review"] = json!(false);
    ready["profiles"]["profiles"][0]["trusted_local_endpoint"] = json!(true);
    let ready: Configuration = serde_json::from_value(ready).unwrap();
    ready.save_fixture_at(&root, &imported).unwrap();
    workspace.set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());
    let _ = summary(&mut workspace, manager, Event::Settings);
    let _ = summary(&mut workspace, manager, Event::UseImportedProfile(false));
    let _ = summary(&mut workspace, manager, Event::Enable(true));
    let _ = summary(&mut workspace, manager, Event::SaveSettings);
    let disk = Queue::load(&root).unwrap();
    assert_eq!(
        disk.preferences().recording_profile.as_ref().unwrap().id,
        ProfileRef::Numeric(1)
    );
    assert_eq!(
        disk.preferences().automation_profile,
        Some(AutomationProfile::Local)
    );
    assert!(disk.preferences().reviewed_grant().is_none());
    assert_eq!(endpoint.accept().unwrap_err().kind(), ErrorKind::WouldBlock);

    workspace.register_journal_summaries(
        Arc::clone(&capture),
        ConnectionConfig {
            ai_summaries: true,
            ..Default::default()
        },
        Arc::new(Redactor::for_journal(
            Secrets::default().seal().unwrap(),
            Default::default(),
        )),
        "Imported recording".into(),
    );
    assert_eq!(
        workspace.journal_summaries.queue.pending()[0]
            .profile
            .as_ref()
            .unwrap()
            .id,
        ProfileRef::Numeric(1)
    );
    finish_capture(&capture);
    let server = thread::spawn(move || {
        for content in [
            r#"{"ready":true}"#,
            r#"{"title":"Checked local model","summary":"Reviewed local recording.","keywords":[]}"#,
            r#"{"title":"Local closing summary","summary":"Reviewed the build.","keywords":[]}"#,
        ] {
            let mut stream = accept(&endpoint);
            let body = request(&mut stream);
            assert_eq!(body["model"], "reviewed-recording-model");
            assert!(!body.to_string().contains("fixture-private"));
            stream
                .get_mut()
                .write_all(fixture_response("200 OK", "", content).as_bytes())
                .unwrap();
        }
    });
    let task = workspace.start_next_journal_summary();
    let mut actions = iced_runtime::task::into_stream(task).unwrap();
    finish_pass(&mut workspace, &mut actions);
    server.join().unwrap();
    assert!(
        source
            .read(None)
            .unwrap()
            .unwrap()
            .timeline
            .iter()
            .any(|entry| entry.kind == "SESSION_SUMMARY" && entry.title == "Local closing summary")
    );
}
