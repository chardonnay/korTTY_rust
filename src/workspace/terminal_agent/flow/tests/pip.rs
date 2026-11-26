//! PiP lifecycle and actual native layout, independent of agent execution.

use iced::advanced::{
    clipboard,
    widget::{Id, Operation, operation},
};
use iced::keyboard::{
    self, Location, Modifiers,
    key::{self, Named},
};
use iced::{Element, Event, Font, Point, Rectangle, Renderer, Size, Vector, mouse};
use iced_runtime::{UserInterface, user_interface::Cache};

use crate::terminal_agent::design::{Arrangement, History, Status};

use super::*;
use design::pip::{Corner, Message as Pip};
use designs::{Histories, inspect_design, terminal_bounds};

#[test]
fn picture_in_picture_adopts_one_history_without_resizing_terminal_or_authorizing_work() {
    let mut fixture = Fixture::new(false, false);
    let id = fixture.open_ask_with_source_tab_visible();
    let run = fixture.run_mut(id);
    run.design.arrangement = Arrangement::PictureInPicture;
    run.design.history = History::Timeline;
    run.design.status = Status::Hidden;
    run.record("Reviewed activity".into());
    let epoch = run.epoch;
    let stream = fixture.workspace.sessions[&fixture.session].stream_generation;
    let size = Size::new(800.0, 500.0);
    assert_eq!(terminal_bounds(&fixture, stream, size).size(), size);
    assert_eq!(
        inspect_design(&fixture, stream, size, Histories::default())
            .0
            .len(),
        1
    );
    no_task(fixture.send(id, Message::Pip(Pip::Fold)));
    assert!(
        inspect_design(&fixture, stream, size, Histories::default())
            .0
            .is_empty()
    );
    no_task(fixture.send(id, Message::Pip(Pip::Fold)));
    no_task(fixture.send(id, Message::Pip(Pip::Hide)));
    assert!(
        inspect_design(&fixture, stream, size, Histories::default())
            .0
            .is_empty()
    );
    no_task(fixture.send(id, Message::Pip(Pip::Show)));
    no_task(fixture.send(id, Message::Pip(Pip::Corner(Corner::TopLeft))));
    let histories = inspect_design(&fixture, stream, size, Histories::default()).0;
    assert_eq!(histories.len(), 1);
    assert!(histories[0].x < size.width / 2.0 && histories[0].y < size.height / 2.0);
    assert_eq!(fixture.run(id).epoch, epoch);
    assert!(!fixture.run(id).cancel.requested());
    // Its independently selected status still occupies its normal right-hand dock.
    fixture.run_mut(id).design.status = Status::Minimap;
    assert!(terminal_bounds(&fixture, stream, size).width < size.width);
    assert_eq!(terminal_bounds(&fixture, stream + 1, size).size(), size);
}

#[test]
fn picture_in_picture_detaches_with_separate_identity_and_close_only_reattaches() {
    let mut fixture = Fixture::new(false, false);
    let id = fixture.open_ask_with_source_tab_visible();
    fixture.run_mut(id).design.arrangement = Arrangement::PictureInPicture;
    let epoch = fixture.run(id).epoch;
    let task = fixture.send(id, Message::Pip(Pip::Detach));
    assert!(task.units() > 0);
    let detached = fixture.run(id).presentation.pip.detached.unwrap();
    assert!(
        !fixture
            .workspace
            .terminal_agents
            .presented_runs
            .contains_key(&fixture.session)
    );
    assert_ne!(detached, id);
    assert!(fixture.workspace.agent_window_view(detached).is_some());
    assert_eq!(
        fixture.workspace.terminal_agents.title(detached),
        Some("kortty · Agent history")
    );
    let close = fixture.workspace.close_terminal_agent(detached).unwrap();
    assert!(close.units() > 0);
    assert!(fixture.run(id).presentation.pip.detached.is_none());
    assert!(!fixture.run(id).cancel.requested());
    assert_eq!(fixture.run(id).epoch, epoch);
    assert!(fixture.workspace.agent_window_view(detached).is_none());
}

#[derive(Default)]
struct LogTableLayout {
    grid: Option<(Rectangle, Vector)>,
    vertical_scroll: Option<(Rectangle, Vector, f32)>,
    labels: Vec<(Rectangle, String)>,
    step_actions: Option<Rectangle>,
    detail_editor: Option<Rectangle>,
}

impl Operation for LogTableLayout {
    fn traverse(&mut self, visit: &mut dyn FnMut(&mut dyn Operation)) {
        visit(self);
    }

    fn scrollable(
        &mut self,
        _: Option<&Id>,
        bounds: Rectangle,
        content: Rectangle,
        translation: Vector,
        _: &mut dyn operation::Scrollable,
    ) {
        if content.height > bounds.height {
            self.vertical_scroll = Some((bounds, translation, content.height - bounds.height));
        }
        if content.width > bounds.width && content.height > bounds.height {
            self.grid = Some((bounds, translation));
        }
    }

    fn text(&mut self, _: Option<&Id>, bounds: Rectangle, value: &str) {
        self.labels.push((bounds, value.to_owned()));
    }

    fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
        if id == Some(&Id::new("agent-log-step-actions")) {
            self.step_actions = Some(bounds);
        }
        if id == Some(&Id::new("agent-log-detail-editor")) {
            self.detail_editor = Some(bounds);
        }
    }
}

fn named_key(name: Named, repeat: bool) -> Event {
    modified_key(name, Modifiers::empty(), repeat)
}

fn modified_key(name: Named, modifiers: Modifiers, repeat: bool) -> Event {
    let key = keyboard::Key::Named(name);
    Event::Keyboard(keyboard::Event::KeyPressed {
        key: key.clone(),
        modified_key: key,
        physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
        location: Location::Standard,
        modifiers,
        text: None,
        repeat,
    })
}

fn release_key(name: Named) -> Event {
    let key = keyboard::Key::Named(name);
    Event::Keyboard(keyboard::Event::KeyReleased {
        key: key.clone(),
        modified_key: key,
        physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
        location: Location::Standard,
        modifiers: Modifiers::empty(),
    })
}

fn keyboard_log_view(fixture: &Fixture, size: Size) -> Element<'_, Desktop> {
    let terminal = crate::workspace::dialog_placement::escape::on_key_press(
        iced::widget::container(iced::widget::text("Terminal"))
            .width(iced::Length::Fill)
            .height(iced::Length::Fill)
            .into(),
        |key, modifiers| {
            (matches!(
                key,
                keyboard::Key::Named(Named::Tab | Named::Enter | Named::ContextMenu)
            ) || *key == keyboard::Key::Named(Named::F10) && modifiers == Modifiers::SHIFT
                || *key == keyboard::Key::Named(Named::F6)
                    && modifiers == Modifiers::CTRL | Modifiers::SHIFT)
                .then_some(Desktop::Noop)
        },
    );
    let stream = fixture.workspace.sessions[&fixture.session].stream_generation;
    let decorated = fixture.workspace.decorate_agent_terminal(
        fixture.session,
        stream,
        terminal,
        size,
        Size::new(8.0, 16.0),
        true,
    );
    fixture
        .workspace
        .agent_log_keyboard(fixture.owner, decorated)
}

#[test]
fn short_side_log_keeps_keyboard_focus_when_selection_adds_controls() {
    use crate::terminal_agent::design::Approval;
    use design::DockPlacement;
    use design::log_table::{Key as LogKey, Message as LogMessage, Surface};
    use engine::usage::{Attempt, AttemptKind, AttemptOutcome};

    let (mut fixture, id, _) = designs::history_review_fixture(History::LogTable);
    fixture.workspace.window_settings.ai_agent_panel.placement = Some(DockPlacement::Right);
    fixture.workspace.window_settings.ai_agent_panel.width = Some(440);
    let run = fixture.run_mut(id);
    run.design.approval = Approval::BelowActivity;
    run.inline_command("Inspect the working directory");
    run.inline_result(engine::Status::Exited(0), "Fixture environment", "");
    let epoch = run.next(Phase::Thinking, "Checking sample activity");
    assert!(run.presentation.model_usage.accept(
        epoch,
        &Attempt {
            kind: AttemptKind::Initial,
            outcome: AttemptOutcome::Completed,
            elapsed: Duration::from_millis(50),
            input_tokens: Some(11),
            output_tokens: Some(5),
            total_tokens: Some(16),
        },
    ));
    run.commands.front_mut().unwrap().proposal.command =
        "printf 'Owned desktop stdout\\n'; printf 'Owned desktop stderr\\n' >&2".into();
    let review = run.next(Phase::Approval, "Review sample command");

    let size = Size::new(1000.0, 395.0);
    let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
    let mut cache = Cache::default();
    for (input, expected) in [
        (
            modified_key(Named::F6, Modifiers::CTRL | Modifiers::SHIFT, false),
            LogKey::Focus,
        ),
        (named_key(Named::End, false), LogKey::Last),
        (named_key(Named::Enter, false), LogKey::Open),
    ] {
        let mut ui = UserInterface::build(
            keyboard_log_view(&fixture, size),
            size,
            cache,
            &mut renderer,
        );
        let mut messages = Vec::new();
        let _ = ui.update(
            &[input],
            mouse::Cursor::Unavailable,
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        assert!(
            matches!(messages.as_slice(), [Desktop::TerminalAgent(run,
            Message::Design(design::Message::LogTable(LogMessage::Navigate(Surface::Pane, key))))]
            if *run == id && *key == expected),
            "the short dock must receive {expected:?}"
        );
        cache = ui.into_cache();
        for message in messages {
            drop(fixture.workspace.update(message));
        }
    }
    let mut ui = UserInterface::build(
        keyboard_log_view(&fixture, size),
        size,
        cache,
        &mut renderer,
    );
    let mut layout = LogTableLayout::default();
    ui.operate(&renderer, &mut layout);
    assert!(
        layout
            .labels
            .iter()
            .any(|(_, label)| label == "Pending approval")
    );
    assert_eq!(fixture.run(id).epoch, review);
    assert!(fixture.run(id).phase == Phase::Approval);
}

#[test]
fn keyboard_entry_uses_only_the_focused_panes_visible_agent_log() {
    use design::log_table::{Key as LogKey, Message as LogMessage, Surface};

    let mut fixture = Fixture::new(false, false);
    let id = fixture.open_ask_with_source_tab_visible();
    let run = fixture.run_mut(id);
    run.design.arrangement = Arrangement::Normal;
    run.design.history = History::LogTable;
    run.design.status = Status::Hidden;
    run.goal = "Review fixture activity".into();
    run.next(Phase::Thinking, "Fixture activity");
    run.inline_command("fixture keyboard route");
    run.inline_result(engine::Status::Exited(0), "fixture keyboard output", "");
    let epoch = run.epoch;
    let size = Size::new(760.0, 540.0);
    let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
    let events = [
        named_key(Named::Tab, false),
        modified_key(Named::F6, Modifiers::CTRL | Modifiers::SHIFT, false),
        named_key(Named::Enter, false),
    ];
    let send = |fixture: &Fixture, renderer: &mut Renderer| {
        let mut ui = UserInterface::build(
            keyboard_log_view(fixture, size),
            size,
            Cache::default(),
            renderer,
        );
        let mut messages = Vec::new();
        let _ = ui.update(
            &events,
            mouse::Cursor::Unavailable,
            renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        messages
    };
    let messages = send(&fixture, &mut renderer);
    assert!(matches!(messages.as_slice(), [
        Desktop::Noop,
        Desktop::TerminalAgent(focus, Message::Design(design::Message::LogTable(LogMessage::Navigate(Surface::Pane, LogKey::Focus)))),
        Desktop::TerminalAgent(open, Message::Design(design::Message::LogTable(LogMessage::Navigate(Surface::Pane, LogKey::Open)))),
    ] if *focus == id && *open == id));
    for message in messages {
        let _ = fixture.workspace.update(message);
    }
    assert_eq!(fixture.run(id).epoch, epoch);

    fixture.run_mut(id).design.history = History::Timeline;
    assert!(matches!(
        send(&fixture, &mut renderer).as_slice(),
        [Desktop::Noop, Desktop::Noop, Desktop::Noop]
    ));
    fixture.run_mut(id).design.history = History::LogTable;
    let other = fixture.workspace.store_session(
        Session::disconnected_ibm(IbmKind::Tn3270e),
        GridSize::default(),
        14,
    );
    fixture.workspace.add_tab(fixture.owner, other);
    assert!(matches!(
        send(&fixture, &mut renderer).as_slice(),
        [Desktop::Noop, Desktop::Noop, Desktop::Noop]
    ));
}

#[test]
fn detached_log_accepts_tab_and_routes_keys_to_its_owned_run() {
    use design::log_table::{Key as LogKey, Message as LogMessage, Surface};

    let mut fixture = Fixture::new(false, false);
    let id = fixture.open_ask_with_source_tab_visible();
    let run = fixture.run_mut(id);
    run.design.arrangement = Arrangement::PictureInPicture;
    run.design.history = History::LogTable;
    run.goal = "Review detached fixture activity".into();
    run.next(Phase::Thinking, "Fixture activity");
    run.inline_command("fixture detached row");
    let _ = fixture.send(id, Message::Pip(Pip::Detach));
    let detached = fixture.run(id).presentation.pip.detached.unwrap();
    let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
    let mut ui = UserInterface::build(
        fixture.workspace.view(detached),
        Size::new(380.0, 310.0),
        Cache::default(),
        &mut renderer,
    );
    let mut messages = Vec::new();
    let _ = ui.update(
        &[
            named_key(Named::Tab, false),
            release_key(Named::Tab),
            named_key(Named::Enter, false),
            release_key(Named::Enter),
            named_key(Named::Tab, false),
            release_key(Named::Tab),
            named_key(Named::Enter, false),
        ],
        mouse::Cursor::Unavailable,
        &mut renderer,
        &mut clipboard::Null,
        &mut messages,
    );
    let [
        Desktop::TerminalAgent(first, Message::PipOwned(selected, focus)),
        Desktop::TerminalAgent(second, Message::PipOwned(opened, open)),
    ] = messages.as_slice()
    else {
        panic!("expected focus and details messages only");
    };
    assert_eq!(
        (*first, *selected, *second, *opened),
        (detached, id, detached, id)
    );
    assert!(matches!(
        focus.as_ref(),
        Message::Design(design::Message::LogTable(LogMessage::Navigate(
            Surface::Detached,
            LogKey::Focus
        )))
    ));
    assert!(matches!(
        open.as_ref(),
        Message::Design(design::Message::LogTable(LogMessage::Navigate(
            Surface::Detached,
            LogKey::Open
        )))
    ));
}

#[test]
fn log_approval_row_is_masked_and_cannot_follow_a_replaced_review() {
    use design::log_table::{Key as LogKey, Message as LogMessage, Row as LogRow, Surface};

    let mut fixture = Fixture::new(false, false);
    let id = fixture.open(Mode::Run);
    let run = fixture.run_mut(id);
    run.design.arrangement = Arrangement::Normal;
    run.design.history = History::LogTable;
    run.design.status = Status::Hidden;
    run.goal = "Inspect fixture".into();
    run.commands.push_back(ReviewedCommand {
        proposal: Command {
            command: "fixture token=synthetic-unmasked-value".into(),
            purpose: "Inspect fixture".into(),
            risk: engine::Risk::ReadOnly,
            elevation: Elevation::None,
        },
        sudo: None,
    });
    let revision = run.next(Phase::Approval, "Review the fixture command");
    let size = Size::new(760.0, 540.0);
    let stream = fixture.workspace.sessions[&fixture.session].stream_generation;
    let layout =
        |fixture: &Fixture| inspect_design(fixture, stream, size, LogTableLayout::default());
    let open = |revision| {
        Message::Design(design::Message::LogTable(LogMessage::Open(
            LogRow::Approval(revision),
        )))
    };
    let navigate = |key| {
        Message::Design(design::Message::LogTable(LogMessage::Navigate(
            Surface::Pane,
            key,
        )))
    };
    assert!(
        layout(&fixture)
            .labels
            .iter()
            .any(|(_, label)| label == "Waiting")
    );
    let _ = fixture.send(id, navigate(LogKey::Last));
    let _ = fixture.send(id, navigate(LogKey::Open));
    let log = layout(&fixture);
    for expected in ["Pending approval", "fixture token=***"] {
        assert!(log.labels.iter().any(|(_, label)| label.contains(expected)));
    }
    assert!(
        log.labels
            .iter()
            .all(|(_, label)| !label.contains("synthetic-unmasked-value"))
    );
    assert_eq!(fixture.run(id).epoch, revision);
    assert!(fixture.run(id).phase == Phase::Approval);
    assert_eq!(fixture.run(id).commands.len(), 1);

    let run = fixture.run_mut(id);
    run.commands.front_mut().unwrap().proposal.command = "fixture replacement".into();
    let replacement = run.next(Phase::Approval, "Review the replacement command");
    no_task(fixture.send(id, open(revision)));
    assert!(
        layout(&fixture)
            .labels
            .iter()
            .all(|(_, label)| label != "Pending approval")
    );
    no_task(fixture.send(id, open(replacement)));
    assert!(
        layout(&fixture)
            .labels
            .iter()
            .any(|(_, label)| label == "Pending approval")
    );
    let _ = fixture.send(id, Message::Cancel);
    assert!(
        layout(&fixture)
            .labels
            .iter()
            .all(|(_, label)| label != "Pending approval" && label != "Waiting")
    );
}

#[test]
fn focused_agent_log_table_gets_keys_before_the_embedded_terminal() {
    use design::log_table::{Key as LogKey, Message as LogMessage, Surface};

    let mut fixture = Fixture::new(false, false);
    let id = fixture.open_ask_with_source_tab_visible();
    let run = fixture.run_mut(id);
    run.design.arrangement = Arrangement::Normal;
    run.design.history = History::LogTable;
    run.design.status = Status::Hidden;
    run.goal = "Review fixture".into();
    run.next(Phase::Thinking, "Fixture activity");
    run.inline_command("fixture keyboard row");
    run.inline_result(engine::Status::Exited(0), "selected result", "");
    let epoch = run.epoch;
    let size = Size::new(760.0, 540.0);
    let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
    let (messages, cache) = {
        let mut ui = UserInterface::build(
            keyboard_log_view(&fixture, size),
            size,
            Cache::default(),
            &mut renderer,
        );
        let mut messages = Vec::new();
        let _ = ui.update(
            &[named_key(Named::Enter, false)],
            mouse::Cursor::Unavailable,
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        assert!(matches!(messages.as_slice(), [Desktop::Noop]));
        messages.clear();
        let mut layout = LogTableLayout::default();
        ui.operate(&renderer, &mut layout);
        let cursor = layout
            .labels
            .iter()
            .find(|(_, label)| label == "fixture keyboard row")
            .unwrap()
            .0
            .center();
        let _ = ui.update(
            &[
                Event::Mouse(mouse::Event::CursorMoved { position: cursor }),
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                named_key(Named::Enter, false),
                named_key(Named::Enter, true),
            ],
            mouse::Cursor::Available(cursor),
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        (messages, ui.into_cache())
    };
    assert!(matches!(messages.as_slice(), [
        Desktop::TerminalAgent(first, Message::Design(design::Message::LogTable(LogMessage::Select(_)))),
        Desktop::TerminalAgent(second, Message::Design(design::Message::LogTable(LogMessage::Navigate(Surface::Pane, LogKey::Open))))
    ] if *first == id && *second == id));
    for message in messages {
        let _ = fixture.workspace.update(message);
    }

    let mut ui = UserInterface::build(
        keyboard_log_view(&fixture, size),
        size,
        cache,
        &mut renderer,
    );
    let mut layout = LogTableLayout::default();
    ui.operate(&renderer, &mut layout);
    assert!(layout.detail_editor.is_some());
    assert_eq!(
        fixture.run(id).log_detail_fixture().unwrap().0,
        "selected result"
    );
    let mut messages = Vec::new();
    let _ = ui.update(
        &[
            named_key(Named::Tab, false),
            named_key(Named::Enter, true),
            release_key(Named::Enter),
            named_key(Named::Enter, false),
        ],
        mouse::Cursor::Unavailable,
        &mut renderer,
        &mut clipboard::Null,
        &mut messages,
    );
    assert!(matches!(messages.as_slice(), [Desktop::Noop]));
    assert_eq!(fixture.run(id).epoch, epoch);
}

pub(super) fn log_label_position(
    ui: &mut UserInterface<'_, Desktop, iced::Theme, Renderer>,
    renderer: &Renderer,
    label: &str,
) -> Point {
    let mut layout = LogTableLayout::default();
    ui.operate(renderer, &mut layout);
    layout
        .labels
        .iter()
        .find(|(_, text)| text == label)
        .unwrap_or_else(|| panic!("missing log table label {label}"))
        .0
        .center()
}

pub(super) fn click_log_label(
    ui: &mut UserInterface<'_, Desktop, iced::Theme, Renderer>,
    renderer: &mut Renderer,
    label: &str,
    button: mouse::Button,
) -> Vec<Desktop> {
    let position = log_label_position(ui, renderer, label);
    let mut messages = Vec::new();
    let _ = ui.update(
        &[
            Event::Mouse(mouse::Event::CursorMoved { position }),
            Event::Mouse(mouse::Event::ButtonPressed(button)),
            Event::Mouse(mouse::Event::ButtonReleased(button)),
        ],
        mouse::Cursor::Available(position),
        renderer,
        &mut clipboard::Null,
        &mut messages,
    );
    messages
}

#[test]
fn history_modules_open_the_actions_for_the_exact_recorded_step() {
    use design::log_table::{Message as LogMessage, Row, Surface};

    for (module, label) in [
        (History::Timeline, "printf fixture"),
        (History::PlanExecution, "printf fixture"),
        (History::CardStack, "1 · printf fixture"),
        (History::Conversation, "printf fixture"),
        (History::Tmux, "printf fixture"),
        (History::Transcript, "First receipt"),
    ] {
        let mut fixture = Fixture::new(false, false);
        fixture.workspace.clipboard.policy.internal = true;
        let id = fixture.open_ask_with_source_tab_visible();
        let run = fixture.run_mut(id);
        run.design.history = module;
        run.goal = "Review two steps with identical commands".into();
        run.next(Phase::Thinking, "Fixture activity");
        let first = run.inline_command("printf fixture");
        let result = run.inline_result(engine::Status::Exited(0), "first output", "");
        assert_eq!(first, result);
        run.record("First receipt".into());
        run.bind_activity_step(result);
        let other = run.inline_command("printf fixture");
        assert_ne!(first, other);
        let result = run.inline_result(engine::Status::Exited(0), "different output", "");
        run.record("Second receipt".into());
        run.bind_activity_step(result);
        let size = Size::new(620.0, 440.0);
        let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
        let mut ui = UserInterface::build(
            history_fixture_view(&fixture, id),
            size,
            Cache::default(),
            &mut renderer,
        );
        let mut messages = click_log_label(&mut ui, &mut renderer, label, mouse::Button::Right);
        let cache = ui.into_cache();
        assert!(
            matches!(
                messages.as_slice(),
                [Desktop::TerminalAgent(run, Message::Design(design::Message::LogTable(
                    LogMessage::OpenMenu(Surface::ActivityEntry(_), Some(Row::Step(step)), _)
                )))] if *run == id && Some(*step) == first
            ),
            "{module:?} exposed the wrong menu"
        );
        no_task(fixture.workspace.update(messages.remove(0)));
        let mut ui = UserInterface::build(
            history_fixture_view(&fixture, id),
            size,
            cache,
            &mut renderer,
        );
        let mut messages =
            click_log_label(&mut ui, &mut renderer, "Copy output", mouse::Button::Left);
        drop(ui);
        assert!(matches!(
            messages.as_slice(),
            [Desktop::TerminalAgent(run, Message::DesignAction(action))]
            if *run == id && action.label() == "Copy output"
        ));
        drop(fixture.workspace.update(messages.remove(0)));
        assert_eq!(
            fixture.workspace.clipboard.buffer.get().as_deref(),
            Some("first output")
        );
    }
}

#[test]
fn conversation_discloses_only_the_selected_command_output() {
    let mut fixture = Fixture::new(false, false);
    let id = fixture.open_ask_with_source_tab_visible();
    let run = fixture.run_mut(id);
    run.design.history = History::Conversation;
    run.goal = "Check both commands".into();
    run.next(Phase::Thinking, "Fixture activity");
    run.inline_command("printf fixture");
    run.inline_result(engine::Status::Exited(0), "first output", "");
    run.inline_command("printf fixture");
    run.inline_result(engine::Status::Exited(9), "second output", "");
    let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
    let size = Size::new(460.0, 420.0);
    let mut ui = UserInterface::build(
        history_fixture_view(&fixture, id),
        size,
        Cache::default(),
        &mut renderer,
    );
    let mut initial = LogTableLayout::default();
    ui.operate(&renderer, &mut initial);
    assert!(initial.labels.iter().any(|(_, label)| label == "Exit 9"));
    assert!(
        !initial
            .labels
            .iter()
            .any(|(_, label)| label == "first output")
    );
    let events = click_log_label(
        &mut ui,
        &mut renderer,
        "printf fixture",
        mouse::Button::Left,
    );
    let cache = ui.into_cache();
    assert!(
        matches!(events.as_slice(), [Desktop::TerminalAgent(run, Message::Design(design::Message::ConversationOutput(_)))] if *run == id)
    );
    for event in events {
        drop(fixture.workspace.update(event));
    }
    let mut ui = UserInterface::build(
        history_fixture_view(&fixture, id),
        size,
        cache,
        &mut renderer,
    );
    let mut expanded = LogTableLayout::default();
    ui.operate(&renderer, &mut expanded);
    assert!(
        expanded
            .labels
            .iter()
            .any(|(_, label)| label == "first output")
    );
    assert!(
        !expanded
            .labels
            .iter()
            .any(|(_, label)| label == "second output")
    );
}

#[test]
fn conversation_menus_copy_from_live_and_archived_earlier_turns() {
    use design::log_table::{ConversationSurface, Message as LogMessage, Row, Surface};

    let mut fixture = Fixture::new(false, false);
    fixture.workspace.clipboard.policy.internal = true;
    for (order, output, status) in [
        (1, "archived output", 0),
        (2, "live output", 7),
        (3, "current output", 3),
    ] {
        let id = fixture.open_ask_with_source_tab_visible();
        let run = fixture.run_mut(id);
        run.started_order = order;
        run.design.history = History::Conversation;
        run.goal = format!("Command in turn {order}");
        run.next(Phase::Thinking, "Fixture activity");
        run.inline_command("printf fixture");
        run.inline_result(engine::Status::Exited(status), output, "");
        run.next(Phase::Finished, "Finished");
        if order == 1 {
            drop(fixture.send(id, Message::Close));
            assert!(!fixture.workspace.terminal_agents.runs.contains_key(&id));
            assert!(
                fixture
                    .workspace
                    .terminal_agents
                    .finished_runs
                    .get(id)
                    .is_some()
            );
        }
    }
    let session = fixture.session;
    let stream = fixture.workspace.sessions[&session].stream_generation;
    let (id, _) = fixture
        .workspace
        .terminal_agents
        .agent_activity_for_stream(session, stream)
        .unwrap();
    let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
    let size = Size::new(600.0, 560.0);
    for (order, label, output) in [
        (1, "printf fixture", "archived output"),
        (2, "Exit 7", "live output"),
    ] {
        let mut ui = UserInterface::build(
            history_fixture_view(&fixture, id),
            size,
            Cache::default(),
            &mut renderer,
        );
        let mut events = click_log_label(&mut ui, &mut renderer, label, mouse::Button::Right);
        let cache = ui.into_cache();
        assert!(
            matches!(events.as_slice(), [Desktop::TerminalAgent(panel, Message::Design(design::Message::LogTable(
            LogMessage::OpenMenu(Surface::EarlierConversation(ConversationSurface::Activity, source), Some(Row::Step(_)), _)
        )))] if *panel == id && *source == order)
        );
        drop(fixture.workspace.update(events.remove(0)));
        let mut ui = UserInterface::build(
            history_fixture_view(&fixture, id),
            size,
            cache,
            &mut renderer,
        );
        let mut menu = LogTableLayout::default();
        ui.operate(&renderer, &mut menu);
        for label in [
            "Copy command",
            "Copy output",
            "Copy entry",
            "Open in Snippet Editor…",
        ] {
            assert!(
                menu.labels.iter().any(|(_, entry)| entry == label),
                "missing {label}"
            );
        }
        let mut events =
            click_log_label(&mut ui, &mut renderer, "Copy output", mouse::Button::Left);
        drop(ui);
        assert!(
            matches!(events.as_slice(), [Desktop::TerminalAgent(panel, Message::Design(design::Message::EarlierConversationStep(..)))] if *panel == id)
        );
        drop(fixture.workspace.update(events.remove(0)));
        assert_eq!(
            fixture.workspace.clipboard.buffer.get().as_deref(),
            Some(output)
        );
        assert_eq!(
            fixture
                .workspace
                .terminal_agents
                .agent_activity_for_stream(session, stream)
                .unwrap()
                .0,
            id
        );
    }
}

fn history_fixture_view(fixture: &Fixture, id: window::Id) -> Element<'_, Desktop> {
    design::history_view(
        fixture.run(id),
        None,
        true,
        Some((&fixture.workspace.terminal_agents, id)),
    )
    .map(move |message| Desktop::TerminalAgent(id, message))
}

#[test]
fn card_stack_shows_typed_output_and_keeps_the_answer_in_browsing_order() {
    let mut fixture = Fixture::new(false, false);
    let id = fixture.open_ask_with_source_tab_visible();
    let run = fixture.run_mut(id);
    run.design.history = History::CardStack;
    run.goal = "Inspect the fixture".into();
    run.next(Phase::Thinking, "Reviewing activity");
    run.inline_command("printf earlier");
    run.inline_result(engine::Status::Exited(0), "earlier output", "");
    run.inline_command("printf latest");
    run.inline_result(engine::Status::Exited(7), "discard\none", "two");
    let size = Size::new(760.0, 550.0);
    let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
    let labels = |fixture: &Fixture, renderer: &mut Renderer| {
        let mut ui = UserInterface::build(
            history_fixture_view(fixture, id),
            size,
            Cache::default(),
            renderer,
        );
        let mut layout = LogTableLayout::default();
        ui.operate(renderer, &mut layout);
        layout
            .labels
            .into_iter()
            .map(|(_, label)| label)
            .collect::<Vec<_>>()
    };
    let review = labels(&fixture, &mut renderer);
    assert!(review.iter().any(|label| label == "printf latest"));
    assert!(review.iter().any(|label| label.starts_with("Exit 7 · ")));
    assert!(review.iter().any(|label| label == "one"));
    assert!(review.iter().any(|label| label == "two"));
    assert!(
        review
            .iter()
            .all(|label| label != "discard" && label != "earlier output")
    );

    let run = fixture.run_mut(id);
    run.inline_decision(&engine::Action::Done, "Final", "Verified card answer");
    run.next(Phase::Finished, "Finished");
    let finished = labels(&fixture, &mut renderer);
    assert!(finished.iter().any(|label| label == "Verified card answer"));
    assert!(finished.iter().all(|label| label != "one"));
    let click = |fixture: &Fixture, renderer: &mut Renderer, label| {
        let mut ui = UserInterface::build(
            history_fixture_view(fixture, id),
            size,
            Cache::default(),
            renderer,
        );
        click_log_label(&mut ui, renderer, label, mouse::Button::Left)
            .pop()
            .unwrap()
    };
    let previous = click(&fixture, &mut renderer, "Previous");
    no_task(fixture.workspace.update(previous));
    let browsed = labels(&fixture, &mut renderer);
    assert!(browsed.iter().any(|label| label == "one"));
    assert!(browsed.iter().all(|label| label != "Verified card answer"));
    let next = click(&fixture, &mut renderer, "Next");
    no_task(fixture.workspace.update(next));
    assert!(
        labels(&fixture, &mut renderer)
            .iter()
            .any(|label| label == "Verified card answer")
    );
}

#[test]
fn side_timeline_follows_new_steps_and_the_final_answer() {
    let mut fixture = Fixture::new(false, false);
    let id = fixture.open_ask_with_source_tab_visible();
    let run = fixture.run_mut(id);
    run.design.history = History::Timeline;
    run.goal = "Inspect the fixture".into();
    run.next(Phase::Thinking, "Reviewing activity");
    for index in 0..20 {
        run.inline_command(&format!("printf fixture-{index}"));
        run.inline_result(engine::Status::Exited(0), "fixture output", "");
    }
    run.inline_command("printf current");

    let size = Size::new(370.0, 230.0);
    let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
    let metrics = |ui: &mut UserInterface<'_, Desktop, iced::Theme, Renderer>,
                   renderer: &Renderer| {
        let mut layout = LogTableLayout::default();
        ui.operate(renderer, &mut layout);
        layout
    };
    let mut ui = UserInterface::build(
        history_fixture_view(&fixture, id),
        size,
        Cache::default(),
        &mut renderer,
    );
    let initial = metrics(&mut ui, &renderer);
    let (viewport, translation, end) = initial.vertical_scroll.unwrap();
    assert!((translation.y - end).abs() <= 1.0);
    assert!(initial.labels.iter().any(|(_, label)| {
        label
            .strip_prefix("Running ")
            .and_then(|elapsed| elapsed.strip_suffix(" s"))
            .is_some_and(|seconds| seconds.parse::<u64>().is_ok())
    }));
    let mut messages = Vec::new();
    let _ = ui.update(
        &[Event::Mouse(mouse::Event::WheelScrolled {
            delta: mouse::ScrollDelta::Lines { x: 0.0, y: 2.0 },
        })],
        mouse::Cursor::Available(viewport.center()),
        &mut renderer,
        &mut clipboard::Null,
        &mut messages,
    );
    assert!(messages.is_empty());
    assert!(metrics(&mut ui, &renderer).vertical_scroll.unwrap().1.y < end - 20.0);
    let cache = ui.into_cache();

    fixture
        .run_mut(id)
        .inline_result(engine::Status::Exited(7), "result", "");
    let mut ui = UserInterface::build(
        history_fixture_view(&fixture, id),
        size,
        cache,
        &mut renderer,
    );
    let revised = metrics(&mut ui, &renderer);
    let (_, translation, end) = revised.vertical_scroll.unwrap();
    assert!(translation.y < end - 20.0);
    assert!(
        revised
            .labels
            .iter()
            .any(|(_, label)| label == "Exit 7 · 1 line")
    );
    let cache = ui.into_cache();

    fixture.run_mut(id).inline_command("printf newest");
    let mut ui = UserInterface::build(
        history_fixture_view(&fixture, id),
        size,
        cache,
        &mut renderer,
    );
    let (_, translation, end) = metrics(&mut ui, &renderer).vertical_scroll.unwrap();
    assert!((translation.y - end).abs() <= 1.0);
    let cache = ui.into_cache();

    let run = fixture.run_mut(id);
    run.inline_result(engine::Status::Exited(0), "latest output", "");
    run.inline_decision(&engine::Action::Done, "Final", "Verified timeline answer");
    run.next(Phase::Finished, "Finished");
    let mut ui = UserInterface::build(
        history_fixture_view(&fixture, id),
        size,
        cache,
        &mut renderer,
    );
    let finished = metrics(&mut ui, &renderer);
    let (viewport, translation, end) = finished.vertical_scroll.unwrap();
    assert!((translation.y - end).abs() <= 1.0);
    assert!(finished.labels.iter().any(|(bounds, label)| {
        label == "Verified timeline answer"
            && viewport.contains(Point::new(
                bounds.center().x,
                bounds.center().y - translation.y,
            ))
    }));
}

#[test]
fn plan_execution_shows_latest_typed_command_output_and_final_answer() {
    use design::log_table::{Message as LogMessage, Row, Surface};

    let mut fixture = Fixture::new(false, false);
    let id = fixture.open_ask_with_source_tab_visible();
    let run = fixture.run_mut(id);
    run.design.history = History::PlanExecution;
    run.goal = "Inspect the fixture".into();
    run.next(Phase::Thinking, "Reviewing activity");
    run.inline_command("printf earlier");
    run.inline_result(engine::Status::Exited(0), "earlier output", "");
    let latest = run.inline_command("printf latest").unwrap();
    let size = Size::new(800.0, 680.0);
    let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
    let labels = |fixture: &Fixture, renderer: &mut Renderer| {
        let mut ui = UserInterface::build(
            history_fixture_view(fixture, id),
            size,
            Cache::default(),
            renderer,
        );
        let mut layout = LogTableLayout::default();
        ui.operate(renderer, &mut layout);
        layout
            .labels
            .into_iter()
            .map(|(_, text)| text)
            .collect::<Vec<_>>()
    };
    let mut ui = UserInterface::build(
        history_fixture_view(&fixture, id),
        size,
        Cache::default(),
        &mut renderer,
    );
    let before = log_label_position(&mut ui, &renderer, "$ printf latest");
    let start = Point::new(size.width * 0.42, before.y);
    let end = Point::new(size.width * 0.60, before.y);
    let mut messages = Vec::new();
    for (event, position) in [
        (mouse::Event::ButtonPressed(mouse::Button::Left), start),
        (mouse::Event::CursorMoved { position: end }, end),
        (mouse::Event::ButtonReleased(mouse::Button::Left), end),
    ] {
        let _ = ui.update(
            &[Event::Mouse(event)],
            mouse::Cursor::Available(position),
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
    }
    let cache = ui.into_cache();
    assert!(matches!(
        messages.as_slice(),
        [Desktop::TerminalAgent(owner, Message::Design(design::Message::ResizePlan(_)))]
            if *owner == id
    ));
    let _ = fixture.workspace.update(messages.remove(0));
    let mut ui = UserInterface::build(
        history_fixture_view(&fixture, id),
        size,
        cache,
        &mut renderer,
    );
    let after = log_label_position(&mut ui, &renderer, "$ printf latest");
    assert!((after.x - before.x - (end.x - start.x)).abs() <= 2.0);
    drop(ui);

    let running = labels(&fixture, &mut renderer);
    assert!(running.iter().any(|label| label == "$ printf latest"));
    assert!(
        running
            .iter()
            .any(|label| label.starts_with("◦ Running · "))
    );

    let run = fixture.run_mut(id);
    let result = run.inline_result(
        engine::Status::Exited(7),
        "discard\none\ntwo\nthree\nfour",
        "five",
    );
    assert_eq!(result, Some(latest));
    let run = fixture.run_mut(id);
    run.inline_decision(
        &engine::Action::Input {
            question: "Explain the result".into(),
            answers: Vec::new(),
        },
        "Later question",
        "Explain the result",
    );
    let done = labels(&fixture, &mut renderer);
    assert!(
        done.iter()
            .any(|label| label.starts_with("Exit 7 · ") && label.ends_with(" s"))
    );
    for line in ["one", "two", "three", "four", "five"] {
        assert!(
            done.iter().any(|label| label == line),
            "missing output line {line}"
        );
    }
    assert!(
        done.iter()
            .all(|label| label != "discard" && label != "earlier output")
    );
    let mut ui = UserInterface::build(
        history_fixture_view(&fixture, id),
        size,
        Cache::default(),
        &mut renderer,
    );
    let messages = click_log_label(
        &mut ui,
        &mut renderer,
        "$ printf latest",
        mouse::Button::Right,
    );
    drop(ui);
    assert!(matches!(
        &messages[..],
        [Desktop::TerminalAgent(owner, Message::Design(design::Message::LogTable(
            LogMessage::OpenMenu(Surface::ActivityExecution, Some(Row::Step(step)), _)
        )))] if *owner == id && *step == latest
    ));

    let run = fixture.run_mut(id);
    run.inline_decision(&engine::Action::Done, "Final", "Verified fixture answer");
    run.next(Phase::Finished, "Finished");
    let final_view = labels(&fixture, &mut renderer);
    assert!(
        final_view
            .iter()
            .any(|label| label == "◆ Verified fixture answer")
    );
    assert!(final_view.iter().all(|label| label != "$ printf latest"));
}

#[test]
fn folded_picture_in_picture_title_menu_acts_on_the_displayed_run() {
    use iced::futures::{StreamExt, executor::block_on};
    use iced_runtime::Action::Window;
    use iced_runtime::window::Action::Resize;

    fn view(fixture: &Fixture, detached: Option<window::Id>, size: Size) -> Element<'_, Desktop> {
        if let Some(detached) = detached {
            fixture.workspace.view(detached)
        } else {
            keyboard_log_view(fixture, size)
        }
    }

    for detached in [false, true] {
        let mut fixture = Fixture::new(false, false);
        fixture.workspace.clipboard.policy.internal = true;
        let id = fixture.open_ask_with_source_tab_visible();
        let run = fixture.run_mut(id);
        run.design.arrangement = Arrangement::PictureInPicture;
        run.design.history = History::Transcript;
        run.design.status = Status::Hidden;
        run.goal = "Inspect the fixture".into();
        run.next(Phase::Thinking, "Fixture activity");
        let answer = "Answer from the displayed run";
        run.inline_decision(&engine::Action::Done, "Finished", answer);
        run.next(Phase::Finished, "Finished");
        no_task(fixture.send(id, Message::Pip(Pip::Fold)));
        let (window, bounds) = if detached {
            drop(fixture.send(id, Message::Pip(Pip::Detach)));
            (
                fixture.run(id).presentation.pip.detached,
                Size::new(300.0, 36.0),
            )
        } else {
            (None, Size::new(720.0, 440.0))
        };
        let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
        let mut ui = UserInterface::build(
            view(&fixture, window, bounds),
            bounds,
            Cache::default(),
            &mut renderer,
        );
        let mut messages = click_log_label(&mut ui, &mut renderer, "⋯", mouse::Button::Left);
        let cache = ui.into_cache();
        assert_eq!(messages.len(), 1);
        let opened = fixture.workspace.update(messages.remove(0));
        let menu_bounds = if detached {
            let mut effects = iced_runtime::task::into_stream(opened).unwrap();
            let Some(Window(Resize(owner, size))) = block_on(effects.next()) else {
                panic!("the detached frame should make room for the menu");
            };
            assert_eq!(Some(owner), window);
            assert!(size.height > bounds.height);
            size
        } else {
            no_task(opened);
            bounds
        };
        assert!(!fixture.run(id).presentation.pip.shows_history());
        let mut ui = UserInterface::build(
            view(&fixture, window, menu_bounds),
            menu_bounds,
            cache,
            &mut renderer,
        );
        let mut messages =
            click_log_label(&mut ui, &mut renderer, "Copy answer", mouse::Button::Left);
        drop(ui);
        assert_eq!(messages.len(), 1);
        let closed = fixture.workspace.update(messages.remove(0));
        if detached {
            let mut effects = iced_runtime::task::into_stream(closed).unwrap();
            assert!(
                matches!(block_on(effects.next()), Some(Window(Resize(owner, size)))
                if Some(owner) == window && size == bounds)
            );
        }
        assert_eq!(
            fixture.workspace.clipboard.buffer.get().as_deref(),
            Some(answer)
        );
        assert!(!fixture.run(id).presentation.pip.shows_history());
    }
}

#[test]
fn docked_log_answer_context_menu_copies_the_selected_run_answer() {
    use design::log_table::{Message as LogMessage, Row, Surface};

    let mut fixture = Fixture::new(false, false);
    fixture.workspace.clipboard.policy.internal = true;
    let id = fixture.open_ask_with_source_tab_visible();
    let run = fixture.run_mut(id);
    run.design.arrangement = Arrangement::Normal;
    run.design.history = History::LogTable;
    run.design.status = Status::Hidden;
    run.goal = "Inspect the fixture".into();
    run.next(Phase::Thinking, "Fixture activity");
    let answer = "Reviewed fixture answer";
    run.inline_decision(&engine::Action::Done, "Finished", answer);
    run.next(Phase::Finished, "Finished");

    let size = Size::new(760.0, 540.0);
    let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
    let mut ui = UserInterface::build(
        keyboard_log_view(&fixture, size),
        size,
        Cache::default(),
        &mut renderer,
    );
    let mut messages = click_log_label(&mut ui, &mut renderer, answer, mouse::Button::Right);
    let cache = ui.into_cache();
    assert!(matches!(
        messages.as_slice(),
        [Desktop::TerminalAgent(run, Message::Design(design::Message::LogTable(
            LogMessage::OpenMenu(Surface::Pane, Some(Row::Answer), _)
        )))] if *run == id
    ));
    no_task(fixture.workspace.update(messages.remove(0)));

    let mut ui = UserInterface::build(
        keyboard_log_view(&fixture, size),
        size,
        cache,
        &mut renderer,
    );
    let mut messages = click_log_label(&mut ui, &mut renderer, "Copy answer", mouse::Button::Left);
    drop(ui);
    assert!(matches!(
        messages.as_slice(),
        [Desktop::TerminalAgent(run, Message::DesignAction(action))]
        if *run == id && action.label() == "Copy answer"
    ));
    let _ = fixture.workspace.update(messages.remove(0));
    assert_eq!(
        fixture.workspace.clipboard.buffer.get().as_deref(),
        Some(answer),
    );
}

#[test]
fn docked_log_run_menu_reopens_the_exact_archived_run() {
    let mut fixture = Fixture::new(false, false);
    let archived_id = fixture.open_ask_with_source_tab_visible();
    let archived = fixture.run_mut(archived_id);
    archived.started_order = 1;
    archived.goal = "Inspect previous result".into();
    archived.next(Phase::Thinking, "");
    archived.inline_decision(&engine::Action::Done, "Finished", "Earlier answer");
    archived.next(Phase::Finished, "Finished");
    let _ = fixture.send(archived_id, Message::Close);
    let stream = fixture.workspace.sessions[&fixture.session].stream_generation;
    assert_eq!(
        fixture
            .workspace
            .finished_agent_run_choices(fixture.session, stream),
        [archived_id],
    );

    let current_id = fixture.open_ask_with_source_tab_visible();
    let current = fixture.run_mut(current_id);
    current.started_order = 2;
    current.goal = "Inspect current result".into();
    current.design.arrangement = Arrangement::Normal;
    current.design.history = History::LogTable;
    current.design.status = Status::Hidden;
    current.next(Phase::Thinking, "");
    current.inline_decision(&engine::Action::Done, "Finished", "Current answer");
    let epoch = current.next(Phase::Finished, "Finished");

    let size = Size::new(760.0, 540.0);
    let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
    let mut cache = Cache::default();
    for (label, button) in [
        ("Current answer", mouse::Button::Right),
        ("Earlier runs ›", mouse::Button::Left),
        ("Finished · Inspect previous result", mouse::Button::Left),
    ] {
        let mut ui = UserInterface::build(
            keyboard_log_view(&fixture, size),
            size,
            cache,
            &mut renderer,
        );
        let mut messages = click_log_label(&mut ui, &mut renderer, label, button);
        cache = ui.into_cache();
        assert_eq!(messages.len(), 1, "{label}");
        if label.starts_with("Finished ·") {
            assert!(matches!(
                messages.as_slice(),
                [Desktop::TerminalAgent(id, Message::SelectPresentedRun(source_epoch, selected))]
                if (*id, *source_epoch, *selected) == (current_id, epoch, archived_id)
            ));
        }
        let _ = fixture.workspace.update(messages.remove(0));
    }
    let (shown_id, shown) = fixture
        .workspace
        .terminal_agents
        .agent_activity_for_stream(fixture.session, stream)
        .expect("the terminal presents the reopened run");
    assert_ne!(shown_id, archived_id);
    assert_ne!(shown_id, current_id);
    assert!(shown.reopened_history);
    assert_eq!(
        shown.status_transcript().unwrap().0.final_answer,
        "Earlier answer"
    );
}

#[test]
fn keyboard_opens_the_focused_log_menu_without_leaking_action_key_releases() {
    use design::log_table::{Key as LogKey, Message as LogMessage, Surface};
    use iced::event::Status as EventStatus;

    let mut fixture = Fixture::new(false, false);
    fixture.workspace.clipboard.policy.internal = true;
    let id = fixture.open_ask_with_source_tab_visible();
    let run = fixture.run_mut(id);
    run.design.arrangement = Arrangement::Normal;
    run.design.history = History::LogTable;
    run.design.status = Status::Hidden;
    run.goal = "Inspect the log".into();
    let epoch = run.next(Phase::Thinking, "");
    run.inline_command("first fixture command");
    run.inline_result(engine::Status::Exited(0), "first fixture result", "");
    run.inline_command("second fixture command");
    run.inline_result(engine::Status::Exited(0), "second fixture result", "");

    let size = Size::new(760.0, 540.0);
    let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
    let mut cache = Cache::default();
    let mut key_event = |fixture: &Fixture, input| {
        let mut ui = UserInterface::build(
            keyboard_log_view(fixture, size),
            size,
            std::mem::take(&mut cache),
            &mut renderer,
        );
        let mut messages = Vec::new();
        let (_, status) = ui.update(
            &[input],
            mouse::Cursor::Unavailable,
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        cache = ui.into_cache();
        (messages, status)
    };
    let shift_f10 = || modified_key(Named::F10, Modifiers::SHIFT, false);
    assert!(matches!(
        key_event(&fixture, shift_f10()).0.as_slice(),
        [Desktop::Noop]
    ));
    for (input, expected) in [
        (
            modified_key(Named::F6, Modifiers::CTRL | Modifiers::SHIFT, false),
            LogKey::Focus,
        ),
        (named_key(Named::End, false), LogKey::Last),
    ] {
        let (mut messages, status) = key_event(&fixture, input);
        assert_eq!(status, [EventStatus::Captured]);
        assert!(matches!(
            messages.as_slice(),
            [Desktop::TerminalAgent(owner, Message::Design(design::Message::LogTable(
                LogMessage::Navigate(Surface::Pane, key)
            )))] if *owner == id && *key == expected
        ));
        let _ = fixture.workspace.update(messages.remove(0));
    }
    let (mut messages, status) = key_event(&fixture, shift_f10());
    assert_eq!(status, [EventStatus::Captured]);
    assert!(matches!(
        messages.as_slice(),
        [Desktop::TerminalAgent(owner, Message::Design(design::Message::LogTable(
            LogMessage::KeyboardMenu(Surface::Pane, _)
        )))] if *owner == id
    ));
    let _ = fixture.workspace.update(messages.remove(0));
    assert_eq!(
        key_event(&fixture, release_key(Named::F10)).1,
        [EventStatus::Captured]
    );

    let (mut messages, status) = key_event(&fixture, named_key(Named::Enter, false));
    assert_eq!(status, [EventStatus::Captured]);
    assert!(matches!(
        messages.as_slice(),
        [Desktop::TerminalAgent(owner, Message::DesignAction(action))]
        if *owner == id && action.label() == "Copy command"
    ));
    let _ = fixture.workspace.update(messages.remove(0));
    let (messages, status) = key_event(&fixture, release_key(Named::Enter));
    assert!(messages.is_empty());
    assert_eq!(status, [EventStatus::Captured]);
    assert_eq!(
        fixture.workspace.clipboard.buffer.get().as_deref(),
        Some("second fixture command")
    );
    assert_eq!(fixture.run(id).epoch, epoch);
}

#[test]
fn log_detail_editor_copies_a_read_only_selection_and_rechecks_its_full_source() {
    use crate::terminal_logging::{Redactor, Secrets};
    use design::log_table::{Message as LogMessage, Row};
    use iced::widget::text_editor::{Action, Edit, Motion};

    let mut fixture = Fixture::new(false, false);
    fixture.workspace.clipboard.policy.internal = true;
    let id = fixture.open_ask_with_source_tab_visible();
    let run = fixture.run_mut(id);
    run.design.history = History::LogTable;
    run.goal = "Review retained output".into();
    run.next(Phase::Thinking, "");
    run.inline_command("fixture output");
    let source = "αβ fixture-source";
    run.inline_result(engine::Status::Exited(0), source, "");
    let step = run.status_transcript().unwrap().0.steps[0].id;
    no_task(fixture.send(
        id,
        Message::Design(design::Message::LogTable(LogMessage::Open(Row::Step(step)))),
    ));
    let (_, revision) = fixture.run(id).log_detail_fixture().unwrap();

    let size = Size::new(760.0, 540.0);
    let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
    let (mut messages, cache) = {
        let mut ui = UserInterface::build(
            fixture.workspace.view(id),
            size,
            Cache::default(),
            &mut renderer,
        );
        let mut layout = LogTableLayout::default();
        ui.operate(&renderer, &mut layout);
        let cursor = layout
            .detail_editor
            .expect("selected details are editable only for selection")
            .center();
        let mut messages = Vec::new();
        let _ = ui.update(
            &[
                Event::Mouse(mouse::Event::CursorMoved { position: cursor }),
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            ],
            mouse::Cursor::Available(cursor),
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        (messages, ui.into_cache())
    };
    assert!(matches!(
        messages.as_slice(),
        [Desktop::TerminalAgent(owner, Message::SelectLogDetail(key, Action::Click(_)))]
        if (*owner, *key) == (id, revision)
    ));
    no_task(fixture.workspace.update(messages.remove(0)));
    for action in [
        Action::Move(Motion::DocumentStart),
        Action::Edit(Edit::Insert('x')),
        Action::Select(Motion::Right),
    ] {
        no_task(fixture.send(id, Message::SelectLogDetail(revision, action)));
    }
    let mut ui = UserInterface::build(fixture.workspace.view(id), size, cache, &mut renderer);
    let key = keyboard::Key::Character("c".into());
    let copy = Event::Keyboard(keyboard::Event::KeyPressed {
        key: key.clone(),
        modified_key: key,
        physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
        location: Location::Standard,
        modifiers: Modifiers::COMMAND,
        text: None,
        repeat: false,
    });
    let (_, status) = ui.update(
        &[copy],
        mouse::Cursor::Unavailable,
        &mut renderer,
        &mut clipboard::Null,
        &mut messages,
    );
    assert_eq!(status, [iced::event::Status::Captured]);
    drop(ui);
    let [Desktop::TerminalAgent(owner, Message::DesignAction(copy))] = messages.as_slice() else {
        panic!("the native editor must route Copy through the Agent action");
    };
    assert_eq!(*owner, id);
    assert_eq!(copy.label(), "Copy selected");
    let copy = *copy;
    let _ = fixture.send(id, Message::DesignAction(copy));
    assert_eq!(
        fixture.workspace.clipboard.buffer.get().as_deref(),
        Some("α")
    );

    let secrets = Secrets::default();
    secrets.add(source);
    fixture.run_mut(id).redactor =
        Redactor::for_journal(secrets.seal().unwrap(), Default::default());
    fixture.workspace.clipboard.buffer.set("unchanged".into());
    no_task(fixture.send(id, Message::DesignAction(copy)));
    assert_eq!(
        fixture.workspace.clipboard.buffer.get().as_deref(),
        Some("unchanged")
    );
}

#[test]
fn narrow_detached_log_table_scrolls_to_the_exact_step_and_keeps_actions_in_view() {
    use design::log_table::{Message as LogMessage, Surface};

    let mut fixture = Fixture::new(false, false);
    fixture.workspace.clipboard.policy.internal = true;
    let id = fixture.open_ask_with_source_tab_visible();
    let run = fixture.run_mut(id);
    run.design.arrangement = Arrangement::Normal;
    run.design.history = History::LogTable;
    run.goal = "Review fixture activity".into();
    run.next(Phase::Thinking, "Fixture activity");
    for number in 1..=20 {
        run.inline_command(&format!("fixture step {number:02}"));
        run.inline_result(
            engine::Status::Exited(0),
            &format!("fixture output {number:02}"),
            "",
        );
    }
    let epoch = run.epoch;
    let stream = fixture.workspace.sessions[&fixture.session].stream_generation;
    let size = Size::new(340.0, 330.0);
    let (docked, _) = inspect_design(&fixture, stream, size, LogTableLayout::default())
        .grid
        .expect("the narrow docked panel has both scroll axes");
    assert!(docked.width > 0.0 && docked.width <= size.width);
    assert!(docked.height > 0.0);

    fixture.run_mut(id).design.arrangement = Arrangement::PictureInPicture;
    let _ = fixture.send(id, Message::Pip(Pip::Detach));
    let detached = fixture.run(id).presentation.pip.detached.unwrap();
    let terminal_before = terminal_bounds(&fixture, stream, size);
    let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
    let (messages, expected, cache) = {
        let view = fixture.workspace.view(detached);
        let mut ui = UserInterface::build(view, size, Cache::default(), &mut renderer);
        let mut layout = LogTableLayout::default();
        ui.operate(&renderer, &mut layout);
        let (grid, offset) = layout.grid.expect("both scroll axes are available");
        assert_eq!(offset.x, 0.0);
        let initial_y = offset.y;
        assert!(
            initial_y > 0.0,
            "the live table starts at the latest activity"
        );
        let at = grid.center();
        let mut messages = Vec::new();
        let _ = ui.update(
            &[
                Event::Mouse(mouse::Event::CursorMoved { position: at }),
                Event::Mouse(mouse::Event::WheelScrolled {
                    delta: mouse::ScrollDelta::Pixels { x: -240.0, y: 40.0 },
                }),
            ],
            mouse::Cursor::Available(at),
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        let [Desktop::TerminalAgent(window, Message::PipOwned(owned, message))] =
            messages.as_slice()
        else {
            panic!("scrolling should update the detached table's reading state");
        };
        assert_eq!((*window, *owned), (detached, id));
        assert!(matches!(
            message.as_ref(),
            Message::Design(design::Message::LogTable(LogMessage::ReadingHistory(
                Surface::Detached,
                true
            )))
        ));
        let mut layout = LogTableLayout::default();
        ui.operate(&renderer, &mut layout);
        let (grid, offset) = layout.grid.unwrap();
        assert!(offset.x > 0.0 && offset.y > 0.0 && offset.y < initial_y);
        let (target_y, expected) = layout
            .labels
            .iter()
            .filter_map(|(bounds, label)| {
                let y = bounds.center().y - offset.y;
                label.strip_prefix("fixture step ").map(|step| (y, step))
            })
            .find(|(y, _)| *y > grid.y + 6.0 && *y < grid.y + grid.height - 15.0)
            .expect("a scrolled step is visible");
        let at = Point::new(grid.center().x, target_y);
        let _ = ui.update(
            &[
                Event::Mouse(mouse::Event::CursorMoved { position: at }),
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)),
            ],
            mouse::Cursor::Available(at),
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        (
            messages,
            format!("fixture output {expected}"),
            ui.into_cache(),
        )
    };
    let [
        _,
        Desktop::TerminalAgent(window, Message::PipOwned(owned, message)),
    ] = messages.as_slice()
    else {
        panic!("the row click should be routed through the detached run");
    };
    assert_eq!(*window, detached);
    assert_eq!(*owned, id);
    assert!(matches!(
        message.as_ref(),
        Message::Design(design::Message::LogTable(LogMessage::OpenMenu(
            Surface::Detached,
            Some(_),
            _
        )))
    ));
    for message in messages {
        no_task(fixture.workspace.update(message));
    }
    {
        let mut activity = UserInterface::build(
            fixture.workspace.view(id),
            size,
            Cache::default(),
            &mut renderer,
        );
        let mut messages = Vec::new();
        let _ = activity.update(
            &[named_key(Named::Enter, false)],
            mouse::Cursor::Unavailable,
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        assert!(
            !messages.iter().any(|message| matches!(
                message,
                Desktop::TerminalAgent(_, Message::DesignAction(_))
            ))
        );
    }
    let mut cache = cache;
    for (key, expected_action) in [
        (Named::ArrowDown, None),
        (Named::Enter, Some("Copy output")),
        (Named::ContextMenu, None),
        (Named::Escape, None),
        (Named::Enter, None),
    ] {
        let mut ui =
            UserInterface::build(fixture.workspace.view(detached), size, cache, &mut renderer);
        let mut messages = Vec::new();
        let _ = ui.update(
            &[named_key(key, false), release_key(key)],
            mouse::Cursor::Unavailable,
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        cache = ui.into_cache();
        let [Desktop::TerminalAgent(window, Message::PipOwned(owned, message))] =
            messages.as_slice()
        else {
            panic!("the menu key should go to its detached run");
        };
        assert_eq!((*window, *owned), (detached, id));
        match expected_action {
            Some(label) => {
                let Message::DesignAction(action) = message.as_ref() else {
                    panic!("the context menu should select the step action");
                };
                assert_eq!(action.label(), label);
            }
            None => assert!(matches!(
                message.as_ref(),
                Message::Design(design::Message::LogTable(
                    LogMessage::SelectMenuItem(1)
                        | LogMessage::Navigate(Surface::Detached, _)
                        | LogMessage::KeyboardMenu(Surface::Detached, _)
                        | LogMessage::CloseMenu
                ))
            )),
        }
        let _ = fixture.workspace.update(messages.remove(0));
    }
    assert_eq!(
        fixture.workspace.clipboard.buffer.get().as_deref(),
        Some(expected.as_str()),
    );
    let mut ui = UserInterface::build(fixture.workspace.view(detached), size, cache, &mut renderer);
    let mut layout = LogTableLayout::default();
    ui.operate(&renderer, &mut layout);
    assert_eq!(fixture.run(id).log_detail_fixture().unwrap().0, expected);
    let editor = layout
        .detail_editor
        .expect("the selected output has a read-only editor");
    assert!(editor.x >= 0.0 && editor.x + editor.width <= size.width);
    assert!(layout.labels.iter().any(|(bounds, text)| {
        text == "Close details" && bounds.x >= 0.0 && bounds.x + bounds.width <= size.width
    }));
    let actions = layout.step_actions.expect("the selected step has actions");
    assert!(actions.x >= 0.0 && actions.x + actions.width <= size.width);
    let horizontal_offset = layout
        .grid
        .expect("the table still scrolls on both axes")
        .1
        .x;
    assert!(horizontal_offset > 0.0);
    let mut messages = Vec::new();
    let _ = ui.update(
        &[named_key(Named::End, false)],
        mouse::Cursor::Unavailable,
        &mut renderer,
        &mut clipboard::Null,
        &mut messages,
    );
    assert!(
        matches!(messages.as_slice(), [Desktop::TerminalAgent(window, Message::PipOwned(owned, _))]
        if *window == detached && *owned == id)
    );
    let cache = ui.into_cache();
    let task = fixture.workspace.update(messages.remove(0));
    use iced::futures::{StreamExt, executor::block_on};
    let mut task_stream =
        iced_runtime::task::into_stream(task).expect("End reveals the selected row");
    let Some(iced_runtime::Action::Widget(mut operation)) = block_on(task_stream.next()) else {
        panic!("expected the selected row to scroll into view");
    };
    let mut ui = UserInterface::build(fixture.workspace.view(detached), size, cache, &mut renderer);
    let mut activity = UserInterface::build(
        fixture
            .run(id)
            .view(None, true, false, (&fixture.workspace.terminal_agents, id)),
        Size::new(640.0, 400.0),
        Cache::default(),
        &mut renderer,
    );
    let mut activity_before = LogTableLayout::default();
    activity.operate(&renderer, &mut activity_before);
    assert!(
        activity_before
            .labels
            .iter()
            .any(|(_, label)| label == "fixture step 20"),
        "the Activity renders the same run"
    );
    let activity_scroll = activity_before
        .vertical_scroll
        .expect("the Activity Log Table scrolls vertically");
    ui.operate(&renderer, operation.as_mut());
    activity.operate(&renderer, operation.as_mut());
    let operation::Outcome::Chain(mut scroll) = operation.finish() else {
        panic!("the last row needs vertical scrolling");
    };
    ui.operate(&renderer, scroll.as_mut());
    activity.operate(&renderer, scroll.as_mut());
    let mut activity_after = LogTableLayout::default();
    activity.operate(&renderer, &mut activity_after);
    assert_eq!(activity_after.vertical_scroll, Some(activity_scroll));
    let mut layout = LogTableLayout::default();
    ui.operate(&renderer, &mut layout);
    let (grid, offset) = layout.grid.unwrap();
    let last = layout
        .labels
        .iter()
        .find(|(_, label)| label == "fixture step 20")
        .unwrap()
        .0;
    let horizontal_scrollbar_height = 10.0;
    assert!(last.y - offset.y >= grid.y);
    assert!(
        last.y + last.height - offset.y <= grid.y + grid.height - horizontal_scrollbar_height,
        "the selected step must fit above the scrollbar: {last:?}, {offset:?}, {grid:?}"
    );
    assert_eq!(offset.x, horizontal_offset);
    assert_eq!(fixture.run(id).epoch, epoch);
    assert_eq!(terminal_bounds(&fixture, stream, size), terminal_before);
}

#[test]
fn picture_in_picture_tabs_and_detached_cleanup_follow_only_the_exact_stream() {
    let mut fixture = Fixture::new(false, false);
    let first = fixture.open_ask_with_source_tab_visible();
    let second = fixture.open_ask_with_source_tab_visible();
    for id in [first, second] {
        fixture.run_mut(id).design.arrangement = Arrangement::PictureInPicture;
    }
    let stream = fixture.workspace.sessions[&fixture.session].stream_generation;
    no_task(fixture.send(second, Message::Pip(Pip::Select(first))));
    assert_eq!(
        fixture
            .workspace
            .terminal_agents
            .agent_activity_for_stream(fixture.session, stream)
            .unwrap()
            .0,
        first
    );
    let _ = fixture.send(first, Message::Pip(Pip::Detach));
    let detached = fixture.run(first).presentation.pip.detached.unwrap();
    let resize = fixture.send(
        detached,
        Message::PipOwned(first, Box::new(Message::Pip(Pip::Select(second)))),
    );
    assert!(resize.units() > 0);
    assert_eq!(
        fixture.run(second).presentation.pip.detached,
        Some(detached)
    );
    assert!(fixture.run(first).presentation.pip.detached.is_none());
    fixture
        .workspace
        .sessions
        .get_mut(&fixture.session)
        .unwrap()
        .stream_generation += 1;
    no_task(fixture.send(second, Message::Pip(Pip::Select(first))));
    let _ = fixture.workspace.reconcile_terminal_agents();
    assert!(fixture.run(second).presentation.pip.detached.is_none());
    no_task(fixture.send(first, Message::Pip(Pip::Detach)));
    assert!(fixture.run(first).presentation.pip.detached.is_none());
}

#[test]
fn picture_in_picture_can_be_restored_after_following_another_design_but_not_an_old_stream() {
    let mut fixture = Fixture::new(false, false);
    let pip = fixture.open_ask_with_source_tab_visible();
    let other = fixture.open_ask_with_source_tab_visible();
    fixture.run_mut(pip).design.arrangement = Arrangement::PictureInPicture;
    let stream = fixture.workspace.sessions[&fixture.session].stream_generation;
    no_task(fixture.send(pip, Message::Pip(Pip::Show)));
    no_task(fixture.send(pip, Message::Pip(Pip::Select(other))));
    assert_eq!(
        fixture
            .workspace
            .terminal_agents
            .agent_activity_for_stream(fixture.session, stream)
            .unwrap()
            .0,
        other
    );
    no_task(fixture.send(pip, Message::Pip(Pip::Show)));
    assert_eq!(
        fixture
            .workspace
            .terminal_agents
            .agent_activity_for_stream(fixture.session, stream)
            .unwrap()
            .0,
        pip
    );
    no_task(fixture.send(pip, Message::Pip(Pip::Select(other))));
    fixture
        .workspace
        .sessions
        .get_mut(&fixture.session)
        .unwrap()
        .stream_generation += 1;
    no_task(fixture.send(pip, Message::Pip(Pip::Show)));
    assert_eq!(
        fixture.workspace.terminal_agents.presented_runs[&fixture.session],
        (stream, other)
    );
    assert!(!fixture.run(pip).cancel.requested());
}

fn followed(fixture: &Fixture) -> window::Id {
    let stream = fixture.workspace.sessions[&fixture.session].stream_generation;
    fixture
        .workspace
        .terminal_agents
        .agent_activity_for_stream(fixture.session, stream)
        .unwrap()
        .0
}

#[test]
fn picture_in_picture_follows_oldest_input_then_newest_working_unfinished_and_finished() {
    let mut fixture = Fixture::new(false, false);
    let first = fixture.open(Mode::Run);
    let second = fixture.open(Mode::Run);
    let third = fixture.open(Mode::Run);
    let _setup = fixture.open(Mode::Run);
    for (order, id) in [first, second, third].into_iter().enumerate() {
        let run = fixture.run_mut(id);
        run.started_order = order as u64 + 1;
        run.design.arrangement = Arrangement::PictureInPicture;
    }
    fixture.run_mut(first).phase = Phase::Question;
    fixture.run_mut(second).phase = Phase::SudoPassword;
    fixture.run_mut(third).phase = Phase::Running;
    assert_eq!(followed(&fixture), first);
    fixture.run_mut(first).paused = true;
    assert_eq!(
        followed(&fixture),
        first,
        "input keeps priority while paused"
    );
    fixture.run_mut(first).phase = Phase::Finished;
    assert_eq!(followed(&fixture), second);
    fixture.run_mut(second).phase = Phase::Thinking;
    fixture.run_mut(third).paused = true;
    assert_eq!(followed(&fixture), second);
    fixture.run_mut(second).paused = true;
    assert_eq!(followed(&fixture), third);
    fixture.run_mut(third).phase = Phase::Finished;
    assert_eq!(followed(&fixture), second);
    fixture.run_mut(second).phase = Phase::Finished;
    assert_eq!(
        followed(&fixture),
        third,
        "an unstarted setup cannot replace retained history"
    );
}

#[test]
fn picture_in_picture_releases_a_pin_only_after_an_accepted_start_and_tracks_start_order() {
    let mut fixture = Fixture::new(false, false);
    let first = fixture.open(Mode::Run);
    let second = fixture.open(Mode::Run);
    for id in [first, second] {
        fixture.run_mut(id).design.arrangement = Arrangement::PictureInPicture;
    }
    no_task(fixture.send(first, Message::Pip(Pip::Show)));
    no_task(fixture.send(second, Message::Start));
    assert_eq!(followed(&fixture), first, "empty setup was not accepted");
    no_task(fixture.send(second, Message::Goal("Inspect the fixture".into())));
    assert_eq!(
        followed(&fixture),
        first,
        "ordinary setup edits retain the explicit pin"
    );
    let directory = fixture
        .profile
        .0
        .canonicalize()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    fixture.run_mut(second).manual_directory = Some(directory.clone());
    // Dropping these tasks does not poll or launch directory/provider workers.
    let started = fixture.send(second, Message::Start);
    assert!(started.units() > 0);
    drop(started);
    assert!(fixture.run(second).phase == Phase::Directory);
    assert_eq!(followed(&fixture), second);
    assert!(
        !fixture
            .workspace
            .terminal_agents
            .presented_runs
            .contains_key(&fixture.session)
    );
    let second_order = fixture.run(second).started_order;
    fixture.run_mut(second).phase = Phase::Finished;
    no_task(fixture.send(second, Message::Pip(Pip::Show)));
    no_task(fixture.send(
        first,
        Message::Goal("Start the earlier dialog later".into()),
    ));
    fixture.run_mut(first).manual_directory = Some(directory);
    drop(fixture.send(first, Message::Start));
    assert!(fixture.run(first).phase == Phase::Directory);
    assert!(fixture.run(first).started_order > second_order);
    assert_eq!(followed(&fixture), first);
    fixture.run_mut(first).phase = Phase::Finished;
    assert_eq!(
        followed(&fixture),
        first,
        "start order is independent of native window creation"
    );
}

#[test]
fn picture_in_picture_new_input_moves_the_detached_window_and_rejects_old_owner_actions() {
    let mut fixture = Fixture::new(false, false);
    let first = fixture.open(Mode::Run);
    let second = fixture.open(Mode::Run);
    for (order, id) in [first, second].into_iter().enumerate() {
        let run = fixture.run_mut(id);
        run.design.arrangement = Arrangement::PictureInPicture;
        run.started_order = order as u64 + 1;
    }
    fixture.run_mut(first).phase = Phase::Finished;
    let epoch = fixture
        .run_mut(second)
        .next(Phase::Directory, "Reading directory");
    no_task(fixture.send(first, Message::Pip(Pip::Show)));
    no_task(fixture.send(first, Message::Pip(Pip::Fold)));
    drop(fixture.send(first, Message::Pip(Pip::Detach)));
    let detached = fixture.run(first).presentation.pip.detached.unwrap();
    no_task(fixture.send(second, Message::InlineDetails));
    assert_eq!(followed(&fixture), first);
    let directory = LocalDirectory::capture(fixture.profile.0.canonicalize().unwrap()).unwrap();
    drop(fixture.send(second, Message::Directory(epoch, Ok(directory))));
    assert!(fixture.run(second).phase == Phase::ProbeApproval);
    assert_eq!(followed(&fixture), second);
    assert_eq!(
        fixture
            .workspace
            .terminal_agents
            .pip_run_for_window(detached),
        Some(second)
    );
    assert!(!fixture.run(second).presentation.pip.shows_history());
    assert!(fixture.run(first).presentation.pip.detached.is_none());
    no_task(fixture.send(
        detached,
        Message::PipOwned(first, Box::new(Message::Cancel)),
    ));
    no_task(fixture.send(detached, Message::Cancel));
    assert!(!fixture.run(second).cancel.requested());
    no_task(fixture.send(
        detached,
        Message::PipOwned(second, Box::new(Message::Pause)),
    ));
    assert!(fixture.run(second).paused);
    fixture
        .workspace
        .sessions
        .get_mut(&fixture.session)
        .unwrap()
        .stream_generation += 1;
    no_task(fixture.send(
        detached,
        Message::PipOwned(second, Box::new(Message::Cancel)),
    ));
    assert!(
        !fixture.run(second).cancel.requested(),
        "stale source actions fail before reconciliation"
    );
    drop(fixture.workspace.reconcile_agent_pip());
    assert!(
        fixture
            .workspace
            .terminal_agents
            .pip_run_for_window(detached)
            .is_none()
    );
    assert!(
        !fixture
            .workspace
            .terminal_agents
            .presented_runs
            .contains_key(&fixture.session)
    );
}

#[test]
fn picture_in_picture_stays_detached_when_reopened_activity_closes() {
    let mut fixture = Fixture::new(false, false);
    let first = fixture.open(Mode::Run);
    let second = fixture.open(Mode::Run);
    for (order, (id, goal)) in [(first, "Earlier result"), (second, "Recent result")]
        .into_iter()
        .enumerate()
    {
        let run = fixture.run_mut(id);
        run.design.arrangement = Arrangement::PictureInPicture;
        run.started_order = order as u64 + 1;
        run.goal = goal.into();
        run.next(Phase::Thinking, "");
        run.inline_decision(&engine::Action::Done, "Finished", goal);
        run.next(Phase::Finished, "Finished");
    }
    let stream = fixture.workspace.sessions[&fixture.session].stream_generation;
    drop(fixture.send(first, Message::Close));
    assert_eq!(
        fixture
            .workspace
            .finished_agent_run_choices(fixture.session, stream),
        [first]
    );
    assert_eq!(followed(&fixture), second);
    no_task(fixture.send(second, Message::Pip(Pip::Fold)));
    drop(fixture.send(second, Message::Pip(Pip::Detach)));
    let detached = fixture.run(second).presentation.pip.detached.unwrap();

    let epoch = fixture.run(second).epoch;
    drop(fixture.send(second, Message::SelectPresentedRun(epoch, first)));
    let reopened = followed(&fixture);
    assert_ne!(reopened, first);
    assert_eq!(fixture.run(reopened).goal, "Earlier result");
    assert_eq!(
        fixture
            .workspace
            .terminal_agents
            .pip_run_for_window(detached),
        Some(reopened)
    );
    assert!(!fixture.run(reopened).presentation.pip.shows_history());

    drop(fixture.send(reopened, Message::Close));
    assert_eq!(followed(&fixture), second);
    assert_eq!(
        fixture
            .workspace
            .terminal_agents
            .pip_run_for_window(detached),
        Some(second)
    );
    assert!(!fixture.run(second).presentation.pip.shows_history());
    assert!(fixture.workspace.agent_window_view(detached).is_some());
    no_task(fixture.send(
        detached,
        Message::PipOwned(reopened, Box::new(Message::Pip(Pip::Fold))),
    ));
    assert!(!fixture.run(second).presentation.pip.shows_history());

    drop(fixture.send(second, Message::Close));
    assert!(fixture.workspace.agent_window_view(detached).is_none());
}

#[test]
fn picture_in_picture_detachment_follows_completion_without_creating_a_history_pin() {
    let mut fixture = Fixture::new(false, false);
    let first = fixture.open_ask_with_source_tab_visible();
    let second = fixture.open_ask_with_source_tab_visible();
    for (order, id) in [first, second].into_iter().enumerate() {
        let run = fixture.run_mut(id);
        run.design.arrangement = Arrangement::PictureInPicture;
        run.started_order = order as u64 + 1;
    }
    fixture.run_mut(second).phase = Phase::Finished;
    let epoch = fixture
        .run_mut(first)
        .next(Phase::Thinking, "Waiting for a fixture decision");
    drop(fixture.send(first, Message::Pip(Pip::Detach)));
    let detached = fixture.run(first).presentation.pip.detached.unwrap();
    assert!(
        !fixture
            .workspace
            .terminal_agents
            .presented_runs
            .contains_key(&fixture.session)
    );
    let decision = Decision {
        summary: "Done".into(),
        message: "Complete".into(),
        action: Action::Done,
    };
    drop(fixture.send(
        first,
        Message::Decision(epoch, Arc::new(Ok(decision).into())),
    ));
    assert!(fixture.run(first).phase == Phase::Finished);
    assert_eq!(followed(&fixture), second);
    assert_eq!(
        fixture
            .workspace
            .terminal_agents
            .pip_run_for_window(detached),
        Some(second)
    );
    fixture.run_mut(second).design.arrangement = Arrangement::Normal;
    drop(fixture.workspace.reconcile_agent_pip());
    assert!(
        fixture
            .workspace
            .terminal_agents
            .pip_run_for_window(detached)
            .is_none()
    );
}
