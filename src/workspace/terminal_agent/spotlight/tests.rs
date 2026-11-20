use iced::advanced::{clipboard, widget::operation::focusable};
use iced::keyboard::{self, Location, key};
use iced::{Event as IcedEvent, Font, Renderer, Size, mouse, widget};
use iced_runtime::{Action as RuntimeAction, UserInterface, user_interface::Cache};

use crate::workspace::chrome;
use crate::workspace::terminal_agent::{Phase, flow::tests::Fixture};

use super::*;

fn open(fixture: &mut Fixture) -> (window::Id, Task<Desktop>) {
    let (owner, _, _) = fixture.workspace.agent_pane(fixture.session).unwrap();
    let task = fixture
        .workspace
        .update(Desktop::Chrome(chrome::Message::Invoke(
            owner,
            chrome::Action::AgentSpotlight,
        )));
    assert!(
        fixture
            .workspace
            .terminal_agents
            .spotlights
            .contains_key(&owner)
    );
    (owner, task)
}

fn send(workspace: &mut Workspace, owner: window::Id, event: Event) -> Task<Desktop> {
    let message = workspace.terminal_agents.spotlights[&owner].message(owner, event);
    workspace.update(message)
}

#[test]
fn spotlight_recalls_saved_prompts_and_starts_review_on_the_original_stream() {
    use iced::futures::{StreamExt, executor::block_on};

    let mut fixture = Fixture::with_endpoint(false, false, "127.0.0.1:12345");
    let root = fixture.workspace.agent_history_root().unwrap();
    store::remember(&root, "Inspect logs".into(), Mode::Run, false).unwrap();
    let (owner, task) = open(&mut fixture);
    let workspace = &mut fixture.workspace;
    let mut actions = iced_runtime::task::into_stream(task).unwrap();
    while let Some(action) = block_on(actions.next()) {
        if let RuntimeAction::Output(message) = action {
            drop(workspace.update(message));
        }
    }
    drop(send(workspace, owner, Event::Edit("My draft".into())));
    drop(send(workspace, owner, Event::Earlier));
    assert_eq!(
        workspace.terminal_agents.spotlights[&owner].prompt,
        "Inspect logs"
    );
    drop(send(workspace, owner, Event::Later));
    assert_eq!(
        workspace.terminal_agents.spotlights[&owner].prompt,
        "My draft"
    );
    let composer = &workspace.terminal_agents.spotlights[&owner];
    let source = composer.source;
    let choice = composer.profiles[1].clone();
    drop(workspace.key(
        owner,
        Key::Named(Named::Tab),
        Location::Standard,
        Modifiers::empty(),
        None,
    ));
    drop(send(workspace, owner, Event::Later));
    assert!(workspace.terminal_agents.spotlights[&owner].profile == choice);
    assert_eq!(
        workspace.terminal_agents.spotlights[&owner].prompt,
        "My draft"
    );
    drop(workspace.key(
        owner,
        Key::Named(Named::Tab),
        Location::Standard,
        Modifiers::empty(),
        None,
    ));
    drop(send(workspace, owner, Event::Submit));

    assert!(!workspace.terminal_agents.spotlights.contains_key(&owner));
    let run = workspace.terminal_agents.runs.values().next().unwrap();
    assert!(run.mode == Mode::Run && run.phase == Phase::Setup);
    assert!(run.binding.live().unwrap() == source);
    assert!(matches!(choice, Choice::Profile(profile) if run.profile() == Some(&profile)));
    assert_eq!(run.goal, "My draft");
    assert!(run.commands.is_empty());
    assert_eq!(
        store::list(&root, false).unwrap().entries[0].goal,
        "Inspect logs"
    );
}

#[test]
fn spotlight_rejects_stale_submit_and_late_history_after_a_privacy_change() {
    let mut fixture = Fixture::with_endpoint(false, false, "127.0.0.1:12345");
    let root = fixture.workspace.agent_history_root().unwrap();
    store::remember(&root, "Personal prompt".into(), Mode::Run, false).unwrap();
    let (owner, _) = open(&mut fixture);
    let workspace = &mut fixture.workspace;
    let revision = workspace.sessions[&fixture.session]
        .session
        .mcp_input_revision();
    drop(workspace.key(
        owner,
        Key::Character("e".into()),
        Location::Standard,
        Modifiers::empty(),
        Some("e"),
    ));
    assert_eq!(
        workspace.sessions[&fixture.session]
            .session
            .mcp_input_revision(),
        revision
    );
    drop(send(
        workspace,
        owner,
        Event::Edit("Never submit from a stale card".into()),
    ));
    let old = &workspace.terminal_agents.spotlights[&owner];
    let submit = old.message(owner, Event::Submit);
    let history = old.message(
        owner,
        Event::Loaded(Arc::new(
            store::list(&root, false).map_err(|e| e.to_string()),
        )),
    );
    workspace.active_tab_mut(owner).unwrap().incognito = true;
    drop(workspace.update(submit));
    assert!(workspace.terminal_agents.runs.is_empty());
    assert!(!workspace.terminal_agents.spotlights.contains_key(&owner));
    drop(workspace.open_agent_spotlight(owner));
    drop(workspace.update(history));
    assert!(
        workspace.terminal_agents.spotlights[&owner]
            .history
            .is_empty()
    );
    assert!(workspace.terminal_agents.spotlights[&owner].source.private);
}

#[test]
fn spotlight_field_captures_history_enter_and_escape() {
    let mut fixture = Fixture::with_endpoint(false, false, "127.0.0.1:12345");
    let (owner, _) = open(&mut fixture);
    let workspace = &mut fixture.workspace;
    drop(send(
        workspace,
        owner,
        Event::Edit("Review the service status".into()),
    ));
    let field = workspace.terminal_agents.spotlights[&owner].field.clone();
    let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
    let view = workspace.agent_spotlight_view(
        owner,
        fixture.session,
        widget::space()
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
    );
    let mut ui = UserInterface::build(
        view,
        Size::new(640.0, 420.0),
        Cache::default(),
        &mut renderer,
    );
    ui.operate(&renderer, &mut focusable::focus(field));
    for (named, expected) in [
        (Named::ArrowUp, Event::Earlier),
        (Named::ArrowDown, Event::Later),
        (Named::Enter, Event::Submit),
        (Named::Escape, Event::Close),
    ] {
        let mut messages = Vec::new();
        ui.update(
            &[IcedEvent::Keyboard(keyboard::Event::KeyPressed {
                key: Key::Named(named),
                modified_key: Key::Named(named),
                physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
                location: Location::Standard,
                modifiers: Modifiers::empty(),
                text: None,
                repeat: false,
            })],
            mouse::Cursor::Unavailable,
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        assert!(
            matches!(messages.as_slice(), [Desktop::AgentSpotlight(Message { event, .. })] if std::mem::discriminant(event) == std::mem::discriminant(&expected))
        );
    }
}
