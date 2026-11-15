use super::*;
use crate::ai_manager::{Configuration, backup};
use crate::clipboard_policy::ClipboardPolicy;

#[test]
fn journal_summary_settings_save_limits_and_drop_stale_recording_receipts() {
    let (endpoint, _) = listener();
    let (scratch, mut workspace, capture, _) = seeded(endpoint.local_addr().unwrap());
    let root = scratch.0.canonicalize().unwrap();
    let manager = show_manager(&mut workspace);
    let _ = summary(&mut workspace, manager, Event::Settings);
    let _ = summary(&mut workspace, manager, Event::Enable(true));
    let _ = summary(&mut workspace, manager, Event::CustomEvidence(true));
    let _ = summary(&mut workspace, manager, Event::EvidenceLines("0".into()));
    let _ = summary(&mut workspace, manager, Event::EvidenceTokens("999".into()));
    let _ = summary(&mut workspace, manager, Event::SaveSettings);
    assert!(
        workspace
            .journal_summaries
            .status
            .as_deref()
            .unwrap()
            .contains("displayed ranges")
    );
    assert!(
        Queue::load(&root)
            .unwrap()
            .preferences()
            .evidence_limits
            .is_none()
    );

    let _ = summary(
        &mut workspace,
        manager,
        Event::EvidenceTokens("1500".into()),
    );
    let _ = summary(&mut workspace, manager, Event::SaveSettings);
    let expected = Some(EvidenceLimits {
        max_lines_per_direction: 0,
        context_tokens: 1500,
    });
    assert_eq!(
        Queue::load(&root).unwrap().preferences().evidence_limits,
        expected
    );
    assert_eq!(
        workspace.automation_summary_evidence_limits(None, None),
        expected
    );
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
        "Recording fixture".into(),
    );
    assert_eq!(
        workspace
            .journal_summaries
            .queue
            .pending()
            .front()
            .unwrap()
            .evidence_limits,
        expected
    );
    assert_eq!(workspace.journal_summaries.live.len(), 1);

    let _ = summary(&mut workspace, manager, Event::Settings);
    let _ = summary(&mut workspace, manager, Event::CustomEvidence(false));
    let _ = summary(&mut workspace, manager, Event::SaveSettings);
    assert!(Queue::load(&root).unwrap().pending().is_empty());
    assert!(workspace.journal_summaries.live.is_empty());
    assert!(
        workspace
            .automation_summary_evidence_limits(None, None)
            .is_none()
    );
    assert_eq!(endpoint.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    finish_capture(&capture);
}

#[test]
fn imported_java_evidence_limits_require_explicit_current_adoption() {
    let (endpoint, _) = listener();
    let (scratch, mut workspace, capture, _) = seeded(endpoint.local_addr().unwrap());
    let root = scratch.0.canonicalize().unwrap();
    let config = |lines: u32| {
        let xml = format!(
            r#"<globalSettings>
            <sessionJournalAiMaxLines>{lines}</sessionJournalAiMaxLines>
            <sessionJournalAiTokenBudget>2400</sessionJournalAiTokenBudget>
            <sessionJournalAiChunkingEnabled>true</sessionJournalAiChunkingEnabled>
        </globalSettings>"#
        );
        let (portable, _) = backup::portable_java_profiles(xml.as_bytes()).unwrap();
        let merged = backup::merge_java_profiles(None, &portable, &ClipboardPolicy::default())
            .unwrap()
            .unwrap();
        serde_json::from_slice::<Configuration>(&merged).unwrap()
    };
    workspace.set_test_ai_configuration(config(17));
    let manager = show_manager(&mut workspace);
    let _ = summary(&mut workspace, manager, Event::Settings);
    let _ = summary(&mut workspace, manager, Event::SaveSettings);
    let disk = Queue::load(&root).unwrap();
    assert!(disk.preferences().evidence_limits.is_none());
    assert!(!disk.preferences().full_output);

    let _ = summary(&mut workspace, manager, Event::Settings);
    let _ = summary(&mut workspace, manager, Event::UseImportedEvidence);
    assert!(
        Queue::load(&root)
            .unwrap()
            .preferences()
            .evidence_limits
            .is_none()
    );
    workspace.set_test_ai_configuration(config(23));
    let _ = summary(&mut workspace, manager, Event::SaveSettings);
    assert_eq!(
        workspace.journal_summaries.status.as_deref(),
        Some(EVIDENCE_IMPORT_CHANGED)
    );
    assert!(
        Queue::load(&root)
            .unwrap()
            .preferences()
            .evidence_limits
            .is_none()
    );

    let _ = summary(&mut workspace, manager, Event::UseImportedEvidence);
    let _ = summary(&mut workspace, manager, Event::SaveSettings);
    let disk = Queue::load(&root).unwrap();
    assert_eq!(
        disk.preferences().evidence_limits,
        Some(EvidenceLimits {
            max_lines_per_direction: 23,
            context_tokens: 2400
        })
    );
    assert!(disk.preferences().full_output);
    assert!(disk.preferences().reviewed_grant().is_none());
    assert!(disk.preferences().automation_grant().is_none());
    finish_capture(&capture);
}
