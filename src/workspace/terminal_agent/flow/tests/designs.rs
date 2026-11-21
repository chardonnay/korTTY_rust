//! Real Iced layout and messages for stream-bound design modules.

use iced::advanced::{
    clipboard,
    widget::{Id, Operation, operation},
};
use iced::widget::{container, text};
use iced::{Event, Font, Length, Point, Rectangle, Renderer, Size, Vector, mouse};
use iced_runtime::{UserInterface, user_interface::Cache};

use crate::terminal_agent::design::{Arrangement, History, Status};

use super::*;

#[test]
fn tmux_numbered_review_uses_the_visible_card_and_rechecks_policy() {
    use crate::clipboard_policy::AiAgentExecution;
    use iced::keyboard::{
        self, Location,
        key::{Code, Physical},
    };

    let mut fixture = Fixture::new(false, false);
    let session =
        Session::start(GridSize::default(), kortty_terminal::TerminalProfile::Xterm).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let frame = loop {
        let frame = session.frame();
        if frame.status == SessionStatus::Running {
            break frame;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(15));
    };
    let pane = fixture
        .workspace
        .sessions
        .get_mut(&fixture.session)
        .unwrap();
    pane.session = session;
    pane.frame = Arc::clone(&frame);
    let stream = pane.stream_generation;
    fixture
        .workspace
        .windows
        .get_mut(&fixture.owner)
        .unwrap()
        .focused = true;

    let id = fixture.open(Mode::Run);
    let checking = fixture.run_mut(id).next(Phase::Directory, "Checking");
    let root = fixture.profile.0.canonicalize().unwrap();
    drop(fixture.send(
        id,
        Message::Directory(checking, Ok(LocalDirectory::capture(root).unwrap())),
    ));
    let run = fixture.run_mut(id);
    run.design.history = History::Tmux;
    run.design.status = Status::Hidden;
    run.goal = "Verify the sample command".into();
    run.initialize_inline();
    run.commands.push_back(ReviewedCommand {
        proposal: Command {
            command: "printf fixture".into(),
            purpose: "Print sample output".into(),
            risk: engine::Risk::RequiresConfirmation,
            elevation: Elevation::None,
        },
        sudo: None,
    });
    run.next(Phase::Approval, "Review sample command");
    let (_, _, source_tab) = fixture.workspace.agent_pane(fixture.session).unwrap();
    fixture
        .workspace
        .select_tab(fixture.owner, source_tab.id, false);
    fixture.workspace.clipboard.policy.ai_agent_execution = AiAgentExecution::Allow;
    assert!(
        fixture.workspace.tmux_review(id, fixture.run(id)).is_some(),
        "the focused source pane should offer the current command review"
    );

    let press_two = |fixture: &Fixture, size: Size, repeat| {
        let pane = fixture.workspace.decorate_agent_terminal(
            fixture.session,
            stream,
            container(text("Terminal"))
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
            size,
            Size::new(8.0, 16.0),
            true,
        );
        let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
        let mut ui = UserInterface::build(pane, size, Cache::default(), &mut renderer);
        let key = keyboard::Key::Character("2".into());
        let mut messages = Vec::new();
        let _ = ui.update(
            &[Event::Keyboard(keyboard::Event::KeyPressed {
                key: key.clone(),
                modified_key: key,
                modifiers: keyboard::Modifiers::empty(),
                physical_key: Physical::Code(Code::Numpad2),
                location: Location::Numpad,
                text: None,
                repeat,
            })],
            mouse::Cursor::Unavailable,
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        messages
    };
    let size = Size::new(1100.0, 600.0);
    assert!(press_two(&fixture, Size::new(360.0, 72.0), false).is_empty());
    assert!(press_two(&fixture, size, true).is_empty());
    let mut messages = press_two(&fixture, size, false);
    assert!(
        matches!(&messages[..], [Desktop::TerminalAgent(actual, Message::TmuxReview(_))] if *actual == id),
        "the visible command card should capture the keypad decision"
    );
    fixture.workspace.clipboard.policy.ai_agent_execution = AiAgentExecution::Confirm;
    no_task(fixture.workspace.update(messages.remove(0)));
    assert!(fixture.run(id).phase == Phase::Approval);
    assert!(fixture.run(id).run_approval.is_none());

    let pane = fixture.workspace.decorate_agent_terminal(
        fixture.session,
        stream,
        container(text("Terminal"))
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
        size,
        Size::new(8.0, 16.0),
        true,
    );
    let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
    let mut ui = UserInterface::build(pane, size, Cache::default(), &mut renderer);
    let mut messages =
        super::pip::click_log_label(&mut ui, &mut renderer, "[2] Cancel", mouse::Button::Left);
    drop(ui);
    assert!(
        matches!(&messages[..], [Desktop::TerminalAgent(actual, Message::TmuxReview(_))] if *actual == id)
    );
    drop(fixture.workspace.update(messages.remove(0)));
    assert!(fixture.run(id).phase == Phase::Cancelled);
}

#[test]
fn tmux_finished_history_can_be_hidden_and_restored_on_its_source_pane() {
    let mut fixture = Fixture::new(false, false);
    let id = fixture.open_ask_with_source_tab_visible();
    let stream = fixture.workspace.sessions[&fixture.session].stream_generation;
    let size = Size::new(1000.0, 540.0);
    let run = fixture.run_mut(id);
    run.design.history = History::Tmux;
    run.design.status = Status::Hidden;
    run.goal = "Check history visibility".into();
    run.initialize_inline();
    run.next(Phase::Finished, "Finished fixture");
    run.presentation
        .finished(std::time::Instant::now() - std::time::Duration::from_secs(30));
    let shown = terminal_bounds(&fixture, stream, size);
    assert!(shown.width < size.width);

    let hide = fixture
        .workspace
        .terminal_agent_design_actions(fixture.session, stream)
        .into_iter()
        .find(|(current, action)| *current == id && action.label() == "Hide Agent history")
        .unwrap()
        .1;
    drop(fixture.send(id, Message::DesignAction(hide)));
    assert_eq!(terminal_bounds(&fixture, stream, size).width, size.width);
    assert!(fixture.run(id).phase == Phase::Finished);
    let _ = fixture.workspace.invoke_chrome(
        fixture.owner,
        crate::workspace::chrome::Action::ToggleAgentTmux,
    );
    assert_eq!(terminal_bounds(&fixture, stream, size), shown);
    assert!(fixture.run(id).phase == Phase::Finished);
}

#[test]
fn expired_status_releases_its_dock_without_removing_the_history() {
    use std::time::{Duration, Instant};

    let mut fixture = Fixture::new(false, false);
    let id = fixture.open_ask_with_source_tab_visible();
    let stream = fixture.workspace.sessions[&fixture.session].stream_generation;
    let size = Size::new(840.0, 540.0);
    let run = fixture.run_mut(id);
    run.design.history = History::LogTable;
    run.design.status = Status::Minimap;
    let shown = terminal_bounds(&fixture, stream, size);
    assert!(shown.width < size.width);
    assert!(shown.height < size.height);

    let run = fixture.run_mut(id);
    run.next(Phase::Finished, "Finished fixture");
    run.presentation
        .finished(Instant::now() - Duration::from_secs(10));
    let hidden = terminal_bounds(&fixture, stream, size);
    assert_eq!(hidden.width, size.width);
    assert_eq!(hidden.height, shown.height);
    let _ = fixture.workspace.invoke_chrome(
        fixture.owner,
        crate::workspace::chrome::Action::ToggleAgentStatus,
    );
    assert_eq!(terminal_bounds(&fixture, stream, size), shown);
}

pub(super) struct TerminalBounds(pub(super) Option<Rectangle>);

impl Operation for TerminalBounds {
    fn traverse(&mut self, visit: &mut dyn FnMut(&mut dyn Operation)) {
        visit(self);
    }
    fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
        if id == Some(&Id::new("agent-design-terminal-fixture")) {
            self.0 = Some(bounds);
        }
    }
}

pub(super) fn terminal_bounds(fixture: &Fixture, stream: u64, size: Size) -> Rectangle {
    inspect_design(fixture, stream, size, TerminalBounds(None))
        .0
        .unwrap()
}

pub(super) fn inspect_design<T: Operation>(
    fixture: &Fixture,
    stream: u64,
    size: Size,
    mut operation: T,
) -> T {
    let terminal = container(text("Terminal"))
        .id("agent-design-terminal-fixture")
        .width(Length::Fill)
        .height(Length::Fill)
        .into();
    let decorated = fixture.workspace.decorate_agent_terminal(
        fixture.session,
        stream,
        terminal,
        size,
        Size::new(8.0, 16.0),
        false,
    );
    let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
    let mut ui = UserInterface::build(decorated, size, Cache::default(), &mut renderer);
    ui.operate(&renderer, &mut operation);
    operation
}

#[derive(Default)]
pub(super) struct Histories(pub(super) Vec<Rectangle>);

impl Operation for Histories {
    fn traverse(&mut self, visit: &mut dyn FnMut(&mut dyn Operation)) {
        visit(self);
    }

    fn scrollable(
        &mut self,
        _: Option<&Id>,
        bounds: Rectangle,
        _: Rectangle,
        _: Vector,
        _: &mut dyn operation::Scrollable,
    ) {
        self.0.push(bounds);
    }
}

#[test]
fn native_adaptive_timeline_reuses_the_selected_timeline_history() {
    let mut fixture = Fixture::new(false, false);
    let id = fixture.open_ask_with_source_tab_visible();
    let stream = fixture.workspace.sessions[&fixture.session].stream_generation;
    let size = Size::new(1280.0, 640.0);
    fixture.run_mut(id).design.history = History::Timeline;
    fixture.run_mut(id).design.status = Status::Hidden;
    let fixed = inspect_design(&fixture, stream, size, Histories::default()).0;
    assert_eq!(fixed.len(), 1);

    fixture.run_mut(id).design.arrangement = Arrangement::Adaptive;
    let adaptive = inspect_design(&fixture, stream, size, Histories::default()).0;
    let [adaptive] = adaptive.as_slice() else {
        panic!("adaptive layout should show one timeline");
    };
    assert_eq!(adaptive.y, fixed[0].y);
    assert_eq!(adaptive.height, fixed[0].height);
    assert_eq!(adaptive.x + adaptive.width, fixed[0].x + fixed[0].width);
    assert_eq!(
        adaptive.width - fixed[0].width,
        crate::horizontal_divider::WIDTH,
        "the adaptive timeline has no resize grip"
    );

    fixture.run_mut(id).design.history = History::Conversation;
    let distinct = inspect_design(&fixture, stream, size, Histories::default()).0;
    assert_eq!(
        distinct.len(),
        2,
        "a distinct history still accompanies the adaptive timeline"
    );
}

#[test]
fn native_agent_docks_restore_terminal_space_after_close_or_stream_replacement() {
    let mut fixture = Fixture::new(false, false);
    let id = fixture.open_ask_with_source_tab_visible();
    let stream = fixture.workspace.sessions[&fixture.session].stream_generation;
    let size = Size::new(800.0, 500.0);
    fixture.run_mut(id).design.history = History::Timeline;
    fixture.run_mut(id).design.status = Status::Hidden;
    let docked = terminal_bounds(&fixture, stream, size);
    assert!(docked.width < size.width - 100.0);
    assert_eq!(docked.height, size.height);
    fixture.run_mut(id).design.history = History::LogTable;
    let bottom = terminal_bounds(&fixture, stream, size);
    assert!(bottom.height < size.height - 100.0);
    assert_eq!(bottom.width, size.width);
    fixture.run_mut(id).closing = true;
    assert_eq!(terminal_bounds(&fixture, stream, size).size(), size);
    fixture.run_mut(id).closing = false;
    assert_eq!(terminal_bounds(&fixture, stream + 1, size).size(), size);
}

#[test]
fn native_adaptive_dock_uses_full_pane_size_and_floating_pill_preserves_terminal() {
    let mut fixture = Fixture::new(false, false);
    let id = fixture.open_ask_with_source_tab_visible();
    let stream = fixture.workspace.sessions[&fixture.session].stream_generation;
    fixture.run_mut(id).design.arrangement = Arrangement::Adaptive;
    let wide = Size::new(160.0 * 8.0, 40.0 * 16.0);
    let first = terminal_bounds(&fixture, stream, wide);
    assert!(first.width < wide.width);
    assert_eq!(first, terminal_bounds(&fixture, stream, wide));
    let narrow = Size::new(60.0 * 8.0, 14.0 * 16.0);
    assert_eq!(terminal_bounds(&fixture, stream, narrow).size(), narrow);
    assert_eq!(first, terminal_bounds(&fixture, stream, wide));
}

#[test]
fn native_status_click_opens_only_its_own_drawer_without_authorizing_work() {
    let mut fixture = Fixture::new(false, false);
    let id = fixture.open_ask_with_source_tab_visible();
    let other = fixture.open_ask_with_source_tab_visible();
    let run = fixture.run_mut(id);
    run.design.status = Status::Spotlight;
    let epoch = run.next(Phase::Thinking, "Fixture activity");
    run.record("Reviewed fixture output".into());
    let stream = fixture.workspace.sessions[&fixture.session].stream_generation;
    let messages = {
        let view = fixture
            .workspace
            .terminal_agents
            .pane_agent_status(fixture.session, stream)
            .unwrap();
        let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
        let mut ui = UserInterface::build(
            view,
            Size::new(420.0, 300.0),
            Cache::default(),
            &mut renderer,
        );
        let point = Point::new(18.0, 18.0);
        let mut messages = Vec::new();
        ui.update(
            &[
                Event::Mouse(mouse::Event::CursorMoved { position: point }),
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            ],
            mouse::Cursor::Available(point),
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        messages
    };
    assert!(
        matches!(messages.as_slice(), [Desktop::TerminalAgent(actual, Message::Design(design::Message::ToggleDrawer))] if *actual == id)
    );
    for message in messages {
        let Desktop::TerminalAgent(actual, message) = message else {
            unreachable!()
        };
        no_task(fixture.send(actual, message));
    }
    assert!(fixture.run(id).presentation.drawer);
    assert!(!fixture.run(other).presentation.drawer);
    assert_eq!(fixture.run(id).epoch, epoch);
    assert!(fixture.run(id).phase == Phase::Thinking);
    assert!(!fixture.run(id).cancel.requested());
}
