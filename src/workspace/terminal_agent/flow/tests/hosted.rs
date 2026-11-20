use super::*;
use crate::workspace::tool_tabs;

#[test]
fn chat_tab_keeps_the_original_pane_and_cancels_its_owned_run_on_close() {
    let mut fixture = Fixture::new(false, false);
    let root = fixture.profile.0.canonicalize().unwrap();
    let baseline = Configuration::load_fixture_from(&root).unwrap();
    let mut json = serde_json::to_value(&baseline).unwrap();
    json["general"]["agent_run_presentation"] = json!("chat_tab");
    let edited: Configuration = serde_json::from_value(json).unwrap();
    edited.save_fixture_at(&root, &baseline).unwrap();
    fixture
        .workspace
        .set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());

    let plan = fixture.open(Mode::Plan);
    assert!(fixture.workspace.hosted_tool_owner(plan).is_none());
    drop(fixture.workspace.close_terminal_agent(plan));

    let id = fixture.open(Mode::Run);
    let tool = fixture.workspace.active_hosted_tab(fixture.owner).unwrap();
    assert_eq!(tool.kind, tool_tabs::Kind::TerminalAgent);
    assert_eq!(tool.instance, id);
    assert!(fixture.workspace.hosted_tool_view(&tool).is_some());
    assert!(fixture.workspace.placed_dialog_owner(id).is_none());
    assert_eq!(
        fixture.run(id).binding.live().unwrap().session,
        fixture.session
    );
    assert_eq!(
        fixture
            .workspace
            .current_run_pane(fixture.run(id))
            .unwrap()
            .0,
        fixture.owner
    );

    fixture.run_mut(id).next(Phase::Thinking, "Running fixture");
    drop(fixture.workspace.request_hosted_tool_close(id));
    assert!(fixture.workspace.hosted_tool_owner(id).is_none());
    assert!(fixture.run(id).closing);
    assert!(fixture.run(id).cancel.requested());
    assert!(fixture.workspace.sessions.contains_key(&fixture.session));
}
