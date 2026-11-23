use super::*;
use crate::workspace::terminal_agent::{Phase, engine, flow::tests::Fixture};

fn fixture() -> Fixture {
    let mut fixture = Fixture::with_endpoint(false, false, "127.0.0.1:12345");
    let catalog = &mut fixture.workspace.window_settings.ai_agent_designs;
    let choice = catalog.duplicate("standard").unwrap();
    let mut layout = catalog.layout(&choice.id).unwrap();
    layout.history = History::Conversation;
    catalog.set_layout(&choice.id, layout);
    assert!(catalog.set_custom_default(&choice.id));
    fixture
}

fn finish(fixture: &mut Fixture, order: u64, goal: &str, answer: &str) -> window::Id {
    let id = fixture.open(Mode::Run);
    let run = fixture.run_mut(id);
    run.started_order = order;
    run.goal = goal.into();
    run.next(Phase::Thinking, "Working");
    run.inline_decision(&engine::Action::Done, "Finished", answer);
    run.next(Phase::Finished, "Finished");
    id
}

#[test]
fn follow_up_creates_one_fresh_agent_setup_on_the_original_stream() {
    let mut fixture = fixture();
    let id = finish(&mut fixture, 1, "First question", "First answer");
    let run = fixture.run_mut(id);
    let source = run.binding.live().unwrap();
    let epoch = run.epoch;
    assert_eq!(run.design.history, History::Conversation);

    drop(fixture.send(id, Message::ContinueConversation(epoch)));
    assert_eq!(fixture.workspace.terminal_agents.runs.len(), 1);
    drop(fixture.send(id, Message::ConversationPrompt(epoch - 1, "stale".into())));
    assert!(fixture.run_mut(id).conversation_prompt.is_empty());
    drop(fixture.send(
        id,
        Message::ConversationPrompt(epoch, "  Follow-up question  ".into()),
    ));
    drop(fixture.send(
        id,
        Message::ConversationPrompt(epoch, "\u{001b}invalid".into()),
    ));
    drop(fixture.send(id, Message::ContinueConversation(epoch)));
    let (&next_id, next) = fixture
        .workspace
        .terminal_agents
        .runs
        .iter()
        .find(|(next_id, _)| **next_id != id)
        .unwrap();
    assert!(next.binding.live().ok() == Some(source));
    assert_eq!(next.goal, "Follow-up question");
    assert_eq!(next.design.history, History::Conversation);
    assert!(next.phase == Phase::Setup && next.model_review.is_none());
    assert!(next.context.is_empty() && next.commands.is_empty());
    assert!(fixture.run_mut(id).conversation_prompt.is_empty());

    drop(fixture.send(id, Message::ConversationPrompt(epoch, "duplicate".into())));
    drop(fixture.send(id, Message::ContinueConversation(epoch)));
    assert_eq!(fixture.workspace.terminal_agents.runs.len(), 2);
    drop(fixture.send(next_id, Message::Close));
    drop(fixture.send(id, Message::ConversationPrompt(epoch, "Try again".into())));
    assert_eq!(fixture.run_mut(id).conversation_prompt, "Try again");
}

#[test]
fn conversation_keeps_retained_answers_in_order_and_rejects_revoked_sources() {
    let mut fixture = fixture();
    let archived = finish(&mut fixture, 1, "First question", "First answer");
    drop(fixture.send(archived, Message::Close));
    finish(&mut fixture, 2, "Second question", "Second answer");
    let latest = finish(&mut fixture, 3, "Third question", "Third answer");
    let epoch = fixture.run_mut(latest).epoch;
    let state = &fixture.workspace.terminal_agents;
    let turns = state.earlier_conversation(&state.runs[&latest]);
    assert_eq!(
        turns
            .iter()
            .map(|turn| turn.transcript().final_answer.as_str())
            .collect::<Vec<_>>(),
        ["First answer", "Second answer"]
    );
    fixture.workspace.revalidate_agent_conversations();
    assert!(fixture.run_mut(latest).conversation_enabled);

    drop(fixture.send(
        latest,
        Message::ConversationPrompt(epoch, "Keep this draft".into()),
    ));
    fixture.workspace.clipboard.policy.ai_allowed = false;
    fixture.workspace.revalidate_agent_conversations();
    assert!(!fixture.run_mut(latest).conversation_enabled);
    drop(fixture.send(latest, Message::ContinueConversation(epoch)));
    assert_eq!(
        fixture.run_mut(latest).conversation_prompt,
        "Keep this draft"
    );
    fixture.workspace.clipboard.policy.ai_allowed = true;

    let session = fixture.session;
    fixture
        .workspace
        .sessions
        .get_mut(&session)
        .unwrap()
        .stream_generation += 1;
    drop(fixture.send(latest, Message::ContinueConversation(epoch)));
    assert_eq!(fixture.workspace.terminal_agents.runs.len(), 2);
    assert_eq!(
        fixture.run_mut(latest).conversation_prompt,
        "Keep this draft"
    );
}

#[test]
fn conversation_field_captures_enter_even_with_an_empty_prompt() {
    use iced::advanced::clipboard;
    use iced::{Event, Font, Point, Renderer, Size, keyboard, mouse};
    use iced_runtime::{UserInterface, user_interface::Cache};

    let mut fixture = fixture();
    let id = finish(&mut fixture, 1, "First question", "First answer");
    fixture.workspace.revalidate_agent_conversations();
    let state = &fixture.workspace.terminal_agents;
    let run = &state.runs[&id];
    let composer = prompt(Activity {
        owner: Some((state, id)),
        surface: Surface::Pane,
        ..run.into()
    })
    .unwrap();
    let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
    let mut ui = UserInterface::build(
        composer,
        Size::new(300.0, 80.0),
        Cache::default(),
        &mut renderer,
    );
    let cursor = mouse::Cursor::Available(Point::new(25.0, 12.0));
    let mut messages = Vec::new();
    ui.update(
        &[
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
        ],
        cursor,
        &mut renderer,
        &mut clipboard::Null,
        &mut messages,
    );
    let enter = keyboard::Key::Named(keyboard::key::Named::Enter);
    let (_, statuses) = ui.update(
        &[Event::Keyboard(keyboard::Event::KeyPressed {
            modified_key: enter.clone(),
            key: enter,
            physical_key: keyboard::key::Physical::Code(keyboard::key::Code::Enter),
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::empty(),
            text: None,
            repeat: false,
        })],
        cursor,
        &mut renderer,
        &mut clipboard::Null,
        &mut messages,
    );
    assert_eq!(statuses, [iced::event::Status::Captured]);
    assert!(
        matches!(messages.as_slice(), [Message::ContinueConversation(epoch)] if *epoch == run.epoch)
    );
}

#[test]
fn retained_command_output_can_open_only_in_its_own_conversation() {
    let mut fixture = fixture();
    let archived = finish(&mut fixture, 1, "First question", "First answer");
    let run = fixture.run_mut(archived);
    let step = run.inline_command("printf fixture").unwrap();
    run.inline_result(engine::Status::Exited(0), "Retained output", "");
    drop(fixture.send(archived, Message::Close));
    let current = finish(&mut fixture, 2, "Second question", "Second answer");
    let key = OutputKey {
        panel: 2,
        turn: 1,
        step,
    };
    let toggle = Message::Design(super::super::Message::ConversationOutput(key));

    drop(fixture.send(current, toggle.clone()));
    assert!(
        fixture
            .run_mut(current)
            .presentation
            .conversation_output
            .contains(&key)
    );
    drop(fixture.send(current, toggle.clone()));
    assert!(
        fixture
            .run_mut(current)
            .presentation
            .conversation_output
            .is_empty()
    );

    let session = fixture.session;
    fixture
        .workspace
        .sessions
        .get_mut(&session)
        .unwrap()
        .stream_generation += 1;
    drop(fixture.send(current, toggle));
    assert!(
        fixture
            .run_mut(current)
            .presentation
            .conversation_output
            .is_empty()
    );
}

#[test]
fn conversation_indicator_stops_for_review_pause_and_reduced_motion() {
    use crate::terminal_agent::design::inline::{Step, StepState};
    use std::time::{Duration, Instant};

    let mut fixture = fixture();
    let id = fixture.open(Mode::Run);
    let run = fixture.run_mut(id);
    run.goal = "Check status".into();
    run.next(Phase::Thinking, "Thinking");
    assert!(run.conversation_pending());
    run.paused = true;
    assert!(!run.conversation_pending());
    run.paused = false;
    run.next(Phase::Approval, "Review command");
    assert!(!run.conversation_pending());
    run.next(Phase::Finished, "Complete");
    assert!(!run.conversation_pending());

    let mut trace = Transcript::default();
    assert_eq!(refresh_interval(&trace, false, false), None);
    assert_eq!(refresh_interval(&trace, true, false), Some(33));
    assert_eq!(refresh_interval(&trace, true, true), None);
    trace.steps.push(Step {
        command: Some("printf fixture".into()),
        started_at: Some(Instant::now()),
        state: StepState::Running,
        ..Step::default()
    });
    assert_eq!(refresh_interval(&trace, true, true), Some(1000));
    let offsets = view::dot_lift(Duration::from_millis(200), false);
    assert!(offsets[0] > offsets[1] && offsets[1] > offsets[2]);
    assert_eq!(view::dot_lift(Duration::from_millis(200), true), [0.0; 3]);
}

#[test]
fn conversation_entrance_uses_the_original_bubble_timestamp_and_motion_preference() {
    use crate::terminal_agent::design::inline::Step;
    use std::time::{Duration, Instant};

    let created = Instant::now();
    let active = created + Duration::from_millis(120);
    let settled = created + Duration::from_millis(300);
    let mut trace = Transcript::default();
    trace.steps.push(Step {
        started_at: Some(created),
        ..Step::default()
    });
    assert_eq!(
        entrance_interval(&trace, None, None, active, false),
        Some(16)
    );
    assert_eq!(entrance_interval(&trace, None, None, active, true), None);
    assert_eq!(entrance_interval(&trace, None, None, settled, false), None);
    trace.steps[0]
        .output
        .push("An existing bubble was updated".into());
    assert_eq!(entrance_interval(&trace, None, None, settled, false), None);
    assert!(view::entrance_scale(Some(Duration::ZERO), false) < 1.0);
    assert_eq!(view::entrance_scale(Some(Duration::ZERO), true), 1.0);
    assert_eq!(
        view::entrance_scale(Some(Duration::from_millis(300)), false),
        1.0
    );
}

#[test]
fn stale_earlier_step_actions_cannot_copy_through_a_replaced_menu_or_stream() {
    let mut fixture = fixture();
    let archived = finish(&mut fixture, 1, "First question", "First answer");
    let run = fixture.run_mut(archived);
    let step = run.inline_command("printf fixture").unwrap();
    let marker = "late archived credential";
    let long_line = "retained ".repeat(400);
    let tail = "c\nd\ne\nf\ng\nh\ni\nj";
    let captured = format!("Retained output {marker}\n{long_line}\n{tail}");
    run.inline_result(engine::Status::Exited(0), &captured, "archived stderr");
    run.inline_command("printf fixture");
    run.inline_result(engine::Status::Exited(0), "different archived step", "");
    let secrets = crate::terminal_logging::Secrets::default();
    secrets.add(marker);
    run.learned_sudo = Some(secrets.seal().unwrap());
    drop(fixture.send(archived, Message::Close));
    let current = finish(&mut fixture, 2, "Second question", "Second answer");
    fixture.workspace.clipboard.policy.internal = true;
    fixture
        .workspace
        .clipboard
        .buffer
        .set("Fixture clipboard".into());
    let key = OutputKey {
        panel: 2,
        turn: 1,
        step,
    };
    let surface = Surface::Pane.earlier_conversation(1).unwrap();
    let row = Some(Row::Step(step));
    let action = Message::Design(super::super::Message::EarlierConversationStep(
        key,
        surface,
        actions::Item::CopyOutput,
    ));
    let open = |surface| {
        Message::Design(super::super::Message::LogTable(
            log_table::Message::OpenMenu(surface, row, iced::Point::ORIGIN),
        ))
    };

    drop(fixture.send(current, action.clone()));
    drop(fixture.send(
        current,
        open(Surface::Detached.earlier_conversation(1).unwrap()),
    ));
    drop(fixture.send(current, action.clone()));
    assert_eq!(
        fixture.workspace.clipboard.buffer.get().as_deref(),
        Some("Fixture clipboard")
    );
    drop(fixture.send(current, open(surface)));
    drop(fixture.send(current, action.clone()));
    assert_eq!(
        fixture.workspace.clipboard.buffer.get().as_deref(),
        Some(format!("Retained output ***\n{long_line}\n{tail}\narchived stderr").as_str()),
    );
    fixture
        .workspace
        .clipboard
        .buffer
        .set("Fixture clipboard".into());
    drop(fixture.send(current, open(surface)));
    let session = fixture.session;
    fixture
        .workspace
        .sessions
        .get_mut(&session)
        .unwrap()
        .stream_generation += 1;
    drop(fixture.send(current, action));
    assert_eq!(
        fixture.workspace.clipboard.buffer.get().as_deref(),
        Some("Fixture clipboard")
    );
}
