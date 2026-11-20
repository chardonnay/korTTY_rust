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
    let id = fixture.open(Mode::Ask);
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
    let id = fixture.open(Mode::Ask);
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
fn keyboard_entry_uses_only_the_focused_panes_visible_agent_log() {
    use design::log_table::{Key as LogKey, Message as LogMessage};

    let mut fixture = Fixture::new(false, false);
    let id = fixture.open(Mode::Ask);
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
        Desktop::TerminalAgent(focus, Message::Design(design::Message::LogTable(LogMessage::Navigate(LogKey::Focus)))),
        Desktop::TerminalAgent(open, Message::Design(design::Message::LogTable(LogMessage::Navigate(LogKey::Open)))),
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
    use design::log_table::{Key as LogKey, Message as LogMessage};

    let mut fixture = Fixture::new(false, false);
    let id = fixture.open(Mode::Ask);
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
            LogKey::Focus
        )))
    ));
    assert!(matches!(
        open.as_ref(),
        Message::Design(design::Message::LogTable(LogMessage::Navigate(
            LogKey::Open
        )))
    ));
}

#[test]
fn log_approval_row_is_masked_and_cannot_follow_a_replaced_review() {
    use design::log_table::{Key as LogKey, Message as LogMessage, Row as LogRow};

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
    let navigate = |key| Message::Design(design::Message::LogTable(LogMessage::Navigate(key)));
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
    use design::log_table::{Key as LogKey, Message as LogMessage};

    let mut fixture = Fixture::new(false, false);
    let id = fixture.open(Mode::Ask);
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
        Desktop::TerminalAgent(second, Message::Design(design::Message::LogTable(LogMessage::Navigate(LogKey::Open))))
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

fn click_log_label(
    ui: &mut UserInterface<'_, Desktop, iced::Theme, Renderer>,
    renderer: &mut Renderer,
    label: &str,
    button: mouse::Button,
) -> Vec<Desktop> {
    let mut layout = LogTableLayout::default();
    ui.operate(renderer, &mut layout);
    let position = layout
        .labels
        .iter()
        .find(|(_, text)| text == label)
        .unwrap_or_else(|| panic!("missing log table label {label}"))
        .0
        .center();
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
fn docked_log_answer_context_menu_copies_the_selected_run_answer() {
    use design::log_table::{Message as LogMessage, Row, Surface};

    let mut fixture = Fixture::new(false, false);
    fixture.workspace.clipboard.policy.internal = true;
    let id = fixture.open(Mode::Ask);
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
    let archived_id = fixture.open(Mode::Ask);
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

    let current_id = fixture.open(Mode::Ask);
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
    let id = fixture.open(Mode::Ask);
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
                LogMessage::Navigate(key)
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
    let id = fixture.open(Mode::Ask);
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
    let id = fixture.open(Mode::Ask);
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
        assert!(messages.is_empty());
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
    let [Desktop::TerminalAgent(window, Message::PipOwned(owned, message))] = messages.as_slice()
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
                        | LogMessage::Navigate(_)
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
    ui.operate(&renderer, operation.as_mut());
    let operation::Outcome::Chain(mut scroll) = operation.finish() else {
        panic!("the last row needs vertical scrolling");
    };
    ui.operate(&renderer, scroll.as_mut());
    let mut layout = LogTableLayout::default();
    ui.operate(&renderer, &mut layout);
    let (grid, offset) = layout.grid.unwrap();
    let last = layout
        .labels
        .iter()
        .find(|(_, label)| label == "fixture step 20")
        .unwrap()
        .0
        .center()
        .y;
    assert!(last - offset.y >= grid.y && last - offset.y <= grid.y + grid.height);
    assert!(offset.x > 0.0);
    assert_eq!(fixture.run(id).epoch, epoch);
    assert_eq!(terminal_bounds(&fixture, stream, size), terminal_before);
}

#[test]
fn picture_in_picture_tabs_and_detached_cleanup_follow_only_the_exact_stream() {
    let mut fixture = Fixture::new(false, false);
    let first = fixture.open(Mode::Ask);
    let second = fixture.open(Mode::Ask);
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
    let pip = fixture.open(Mode::Ask);
    let other = fixture.open(Mode::Ask);
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
fn picture_in_picture_detachment_follows_completion_without_creating_a_history_pin() {
    let mut fixture = Fixture::new(false, false);
    let first = fixture.open(Mode::Ask);
    let second = fixture.open(Mode::Ask);
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
