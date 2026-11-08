use crate::workspace::journal_image_menu::Choice;

use super::super::screenshot_access::{
    Event as Context, Row,
    tests::{Fixture, REFERENCE},
};
use super::*;

fn configure(workspace: &mut Workspace) {
    workspace.window_settings.journal_ai = topic_ai_settings::Preferences {
        enabled: true,
        endpoint: "127.0.0.1:11434".into(),
        model: "fixture-vision".into(),
        vision_enabled: true,
        auto_screenshot_analysis: true,
    };
}

#[test]
fn journal_screenshot_analysis_checks_saved_profile_policy_and_recording_consent() {
    let fixture = Fixture::new();
    let (mut workspace, _, id) = fixture.open(true);
    assert!(!workspace.journal_screenshot_analysis_available());
    configure(&mut workspace);
    assert!(workspace.journal_screenshot_analysis_available());
    assert!(workspace.journal_automatic_screenshot_analysis_enabled());
    assert!(
        workspace
            .screenshot_analysis_profile(Trigger::Automatic {
                tab: 1,
                run: 2,
                consent: 3
            })
            .is_none()
    );
    workspace
        .window_settings
        .journal_ai
        .auto_screenshot_analysis = false;
    assert!(!workspace.journal_automatic_screenshot_analysis_enabled());
    assert!(
        workspace
            .screenshot_analysis_profile(Trigger::Manual(Origin::Viewer(id)))
            .is_some()
    );

    let Mode::Viewer(viewer) = &workspace.journals[&id].mode else {
        panic!("expected viewer");
    };
    let row = Row::new(0, &viewer.document.as_ref().unwrap().timeline[0]).unwrap();
    let _ = workspace.update(Desktop::SessionJournal(
        id,
        Message::ScreenshotAction(Context::ThumbnailMenu(row.clone(), iced::Point::ORIGIN)),
    ));
    workspace.clipboard.policy.ai_allowed = false;
    let _ = workspace.update(Desktop::SessionJournal(
        id,
        Message::ScreenshotAction(Context::ThumbnailAction(row, Choice::Analyze)),
    ));
    assert!(workspace.journal_screenshot_analyses.pending.is_empty());
}

#[test]
fn journal_screenshot_analysis_refreshes_a_current_viewer_with_the_saved_fields() {
    let fixture = Fixture::new();
    let (mut workspace, owner, id) = fixture.open(false);
    configure(&mut workspace);
    let Mode::Viewer(viewer) = &workspace.journals[&id].mode else {
        panic!("expected viewer");
    };
    let source = viewer.source.clone();
    let asset = Arc::new(Screenshot::review(&source, "screen", REFERENCE).unwrap());
    // Test double for a validated model response. Persistence and viewer refresh use real data.
    let analysis = Arc::new(Analysis {
        description: "Two terminal panels".into(),
        tags: vec!["terminal".into(), "panels".into()],
        model: "fixture-vision".into(),
    });
    let saved = publish(&Analyzed { asset, analysis }, None).unwrap();
    assert!(saved.outcome.changed);
    let Mode::Viewer(viewer) = &mut workspace.journals.get_mut(&id).unwrap().mode else {
        panic!("expected viewer");
    };
    // Test double for a more recent metadata event already installed in the Viewer.
    Arc::make_mut(viewer.document.as_mut().unwrap())
        .summary
        .title = "Newer viewer title".into();
    let trigger = Trigger::Manual(Origin::Viewer(id));
    let profile = workspace.screenshot_analysis_profile(trigger).unwrap();
    let ticket = workspace.allocate();
    workspace.journal_screenshot_analyses.pending.insert(
        ticket,
        Request {
            window: owner,
            trigger,
            source,
            entry_id: "screen".into(),
            profile,
            phase: Phase::Saving,
        },
    );
    let _ = workspace.update_screenshot_analysis(Event::Saved(ticket, Ok(Arc::new(saved))));
    let Mode::Viewer(viewer) = &workspace.journals[&id].mode else {
        panic!("expected viewer");
    };
    let row = &viewer.document.as_ref().unwrap().timeline[0];
    assert_eq!(
        viewer.document.as_ref().unwrap().summary.title,
        "Newer viewer title"
    );
    assert_eq!(
        row.screenshot_analysis().unwrap(),
        ["AI analysis", "Two terminal panels", "terminal", "panels"]
    );
    assert_eq!(
        viewer.screenshot_analysis_notice.as_deref(),
        Some("Screenshot analysis saved.")
    );
}

#[test]
fn journal_screenshot_analysis_revocation_cancels_an_active_model_session() {
    let _exclusive = crate::local_ai::test_support::exclusive_run();
    let fixture = Fixture::new();
    let (mut workspace, owner, id) = fixture.open(false);
    configure(&mut workspace);
    let Mode::Viewer(viewer) = &workspace.journals[&id].mode else {
        panic!("expected viewer");
    };
    let source = viewer.source.clone();
    let trigger = Trigger::Manual(Origin::Viewer(id));
    let profile = workspace.screenshot_analysis_profile(trigger).unwrap();
    let (session, cancel) = local_ai::Session::begin(profile.clone()).unwrap();
    let ticket = workspace.allocate();
    workspace.journal_screenshot_analyses.pending.insert(
        ticket,
        Request {
            window: owner,
            trigger,
            source,
            entry_id: "screen".into(),
            profile,
            phase: Phase::Analyzing { _cancel: cancel },
        },
    );
    assert!(session.client().active());
    workspace.window_settings.journal_ai.vision_enabled = false;
    workspace.cancel_obsolete_journal_screenshot_analyses();
    assert!(!session.client().active());
    assert!(workspace.journal_screenshot_analyses.pending.is_empty());
}
