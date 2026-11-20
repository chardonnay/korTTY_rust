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
            (matches!(key, keyboard::Key::Named(Named::Tab | Named::Enter))
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
    assert!(
        layout
            .labels
            .iter()
            .any(|(_, label)| label == "selected result")
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

#[test]
fn narrow_detached_log_table_scrolls_to_the_exact_step_and_keeps_actions_in_view() {
    let mut fixture = Fixture::new(false, false);
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
    let [Desktop::TerminalAgent(window, Message::PipOwned(owned, _))] = messages.as_slice() else {
        panic!("the row click should be routed through the detached run");
    };
    assert_eq!(*window, detached);
    assert_eq!(*owned, id);
    for message in messages {
        no_task(fixture.workspace.update(message));
    }
    let mut ui = UserInterface::build(fixture.workspace.view(detached), size, cache, &mut renderer);
    let mut layout = LogTableLayout::default();
    ui.operate(&renderer, &mut layout);
    for label in [expected.as_str(), "Close details"] {
        assert!(
            layout.labels.iter().any(|(bounds, text)| {
                text == label && bounds.x >= 0.0 && bounds.x + bounds.width <= size.width
            }),
            "{label} must fit in the narrow details pane"
        );
    }
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
