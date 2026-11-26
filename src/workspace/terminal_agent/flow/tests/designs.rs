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
    numbered_history_review(History::Tmux);
}

#[test]
fn conversation_numbered_review_uses_the_visible_bubble_and_rechecks_policy() {
    numbered_history_review(History::Conversation);
}

#[test]
fn timeline_plan_stack_and_log_table_use_the_bound_panel_review() {
    for history in [
        History::Timeline,
        History::PlanExecution,
        History::CardStack,
        History::LogTable,
    ] {
        numbered_history_review(history);
    }
}

pub(super) fn history_review_fixture(history: History) -> (Fixture, window::Id, u64) {
    use crate::clipboard_policy::AiAgentExecution;

    let mut fixture = Fixture::new(false, false);
    let pane = fixture
        .workspace
        .sessions
        .get_mut(&fixture.session)
        .unwrap();
    // A starting shell can repaint between layout and the decision recheck.
    pane.session.set_mirror_test_frame(None);
    pane.frame = pane.session.frame();
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
    run.design.history = history;
    run.design.status = Status::Hidden;
    run.presentation.activity.height = 360.0;
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
    (fixture, id, stream)
}

#[test]
fn docked_log_table_rebuilds_when_command_review_gains_and_loses_focus() {
    let (mut fixture, id, stream) = history_review_fixture(History::LogTable);
    let size = Size::new(760.0, 540.0);
    let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
    let mut cache = Cache::default();

    for focused in [false, true, false, true] {
        fixture
            .workspace
            .windows
            .get_mut(&fixture.owner)
            .unwrap()
            .focused = focused;
        assert_eq!(
            fixture
                .workspace
                .panel_review(id, fixture.run(id), None)
                .is_some(),
            focused
        );
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
        let mut ui = UserInterface::build(pane, size, cache, &mut renderer);
        let control = if focused {
            "[1] Allow once"
        } else {
            "Review in Activity…"
        };
        let messages =
            super::pip::click_log_label(&mut ui, &mut renderer, control, mouse::Button::Left);
        match messages.as_slice() {
            [Desktop::TerminalAgent(run, Message::PanelReview(_))] if focused => {
                assert_eq!(*run, id);
            }
            [Desktop::TerminalAgent(run, Message::Focus)] if !focused => {
                assert_eq!(*run, id);
            }
            _ => panic!("unexpected docked review action while focused={focused}"),
        }
        cache = ui.into_cache();
    }
}

fn numbered_history_review(history: History) {
    use crate::clipboard_policy::AiAgentExecution;
    use iced::keyboard::{
        self, Location,
        key::{Code, Physical},
    };

    let (mut fixture, id, stream) = history_review_fixture(history);
    assert!(
        fixture
            .workspace
            .panel_review(id, fixture.run(id), None)
            .is_some(),
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
    let clipped_height = if matches!(history, History::Tmux | History::Conversation) {
        72.0
    } else {
        24.0
    };
    assert!(
        press_two(&fixture, Size::new(360.0, clipped_height), false).is_empty(),
        "{history:?} must leave numeric keys with the terminal when the review is clipped"
    );
    assert!(press_two(&fixture, size, true).is_empty());
    let mut messages = press_two(&fixture, size, false);
    assert!(
        matches!(&messages[..], [Desktop::TerminalAgent(actual, Message::PanelReview(_))] if *actual == id),
        "the visible {history:?} command card should capture the keypad decision"
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
    let cancel = if history == History::Timeline {
        "2 Cancel"
    } else {
        "[2] Cancel"
    };
    let mut messages =
        super::pip::click_log_label(&mut ui, &mut renderer, cancel, mouse::Button::Left);
    drop(ui);
    assert!(
        matches!(&messages[..], [Desktop::TerminalAgent(actual, Message::PanelReview(_))] if *actual == id),
        "the visible {history:?} command card should handle a click on Cancel"
    );
    drop(fixture.workspace.update(messages.remove(0)));
    assert!(
        fixture.run(id).phase == Phase::Cancelled,
        "the current {history:?} command should be cancelled"
    );
}

#[test]
fn detached_conversation_review_rejects_focus_changes_before_cancelling() {
    use crate::clipboard_policy::AiAgentExecution;

    let (mut fixture, id, _) = history_review_fixture(History::Conversation);
    fixture.run_mut(id).design.arrangement = Arrangement::PictureInPicture;
    // This test clicks layout coordinates and checks focus, independent of entrance motion.
    fixture
        .workspace
        .window_settings
        .ai_agent_design_reduced_animations = Some(true);
    fixture.workspace.clipboard.policy.ai_agent_execution = AiAgentExecution::Confirm;
    drop(fixture.send(id, Message::Pip(design::pip::Message::Detach)));
    let detached = fixture.run(id).presentation.pip.detached.unwrap();
    fixture
        .workspace
        .windows
        .get_mut(&fixture.owner)
        .unwrap()
        .focused = false;
    fixture.workspace.native_focused_window = Some(detached);
    assert!(
        fixture
            .workspace
            .panel_review(id, fixture.run(id), Some(detached))
            .is_some()
    );

    let click_cancel = |fixture: &Fixture| {
        let view = fixture
            .workspace
            .agent_window_view(detached)
            .unwrap()
            .map(move |message| Desktop::TerminalAgent(detached, message));
        let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
        let mut ui = UserInterface::build(
            view,
            Size::new(420.0, 540.0),
            Cache::default(),
            &mut renderer,
        );
        super::pip::click_log_label(&mut ui, &mut renderer, "[2] Cancel", mouse::Button::Left)
            .pop()
            .expect("detached Cancel button should dispatch its action")
    };

    let stale = click_cancel(&fixture);
    fixture.workspace.native_focused_window = None;
    drop(fixture.workspace.update(stale));
    assert!(fixture.run(id).phase == Phase::Approval);

    fixture.workspace.native_focused_window = Some(detached);
    drop(fixture.workspace.update(click_cancel(&fixture)));
    assert!(fixture.run(id).phase == Phase::Cancelled);
}

#[test]
fn every_docked_history_can_be_hidden_and_restored_on_its_source_pane() {
    use crate::workspace::chrome::Action::ToggleAgentHistory;

    for history in [
        History::Timeline,
        History::PlanExecution,
        History::CardStack,
        History::Tmux,
        History::Conversation,
        History::LogTable,
    ] {
        let mut fixture = Fixture::new(false, false);
        let id = fixture.open_ask_with_source_tab_visible();
        let stream = fixture.workspace.sessions[&fixture.session].stream_generation;
        let size = Size::new(1000.0, 540.0);
        let run = fixture.run_mut(id);
        run.design.history = history;
        run.design.status = Status::Hidden;
        run.goal = "Check history visibility".into();
        run.initialize_inline();
        run.next(Phase::Finished, "Finished fixture");
        run.presentation
            .finished(std::time::Instant::now() - std::time::Duration::from_secs(30));
        let shown = terminal_bounds(&fixture, stream, size);
        assert!(
            shown.width < size.width || shown.height < size.height,
            "{history:?}"
        );

        let hide = fixture
            .workspace
            .terminal_agent_design_actions(fixture.session, stream)
            .into_iter()
            .find(|(current, action)| *current == id && action.label() == "Hide Agent history")
            .unwrap()
            .1;
        no_task(fixture.send(id, Message::DesignAction(hide)));
        assert_eq!(
            terminal_bounds(&fixture, stream, size).size(),
            size,
            "{history:?}"
        );
        assert!(fixture.run(id).phase == Phase::Finished);

        let _ = fixture
            .workspace
            .invoke_chrome(fixture.owner, ToggleAgentHistory);
        assert_eq!(
            terminal_bounds(&fixture, stream, size),
            shown,
            "{history:?}"
        );
        assert!(fixture.run(id).phase == Phase::Finished);
    }
}

#[test]
fn card_stack_wheel_browses_older_activity_and_leaves_terminal_scroll_alone() {
    let (mut fixture, id, stream) = history_review_fixture(History::CardStack);
    fixture.workspace.clipboard.policy.ai_agent_execution =
        crate::clipboard_policy::AiAgentExecution::Confirm;
    fixture.run_mut(id).activity.extend([
        "First fixture receipt".into(),
        "Second fixture receipt".into(),
    ]);
    let size = Size::new(1100.0, 650.0);
    let interact = |fixture: &Fixture, label, delta| {
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
        let Some((delta, count)) = delta else {
            return super::pip::click_log_label(&mut ui, &mut renderer, label, mouse::Button::Left);
        };
        let position = super::pip::log_label_position(&mut ui, &renderer, label);
        let mut messages = Vec::new();
        let _ = ui.update(
            &vec![Event::Mouse(mouse::Event::WheelScrolled { delta }); count],
            mouse::Cursor::Available(position),
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        messages
    };
    let before = mouse::ScrollDelta::Lines { x: 0.0, y: 1.0 };
    let after = mouse::ScrollDelta::Pixels { x: 0.0, y: -35.0 };
    assert!(interact(&fixture, "Terminal", Some((before, 1))).is_empty());
    let mut cancel = interact(&fixture, "[2] Cancel", None);
    assert!(
        matches!(&cancel[..], [Desktop::TerminalAgent(owner, Message::PanelReview(_))] if *owner == id)
    );
    let messages = interact(&fixture, "Previous", Some((before, 2)));
    assert_eq!(messages.len(), 2);
    for message in messages {
        assert!(matches!(
            &message,
            Desktop::TerminalAgent(owner, Message::Design(design::Message::ScrollCards { direction: 1, review: true })) if *owner == id
        ));
        no_task(fixture.workspace.update(message));
    }
    assert!(
        fixture
            .workspace
            .panel_review(id, fixture.run(id), None)
            .is_none()
    );
    no_task(fixture.workspace.update(cancel.remove(0)));
    assert!(fixture.run(id).phase == Phase::Approval);

    let messages = interact(&fixture, "First fixture receipt", Some((after, 2)));
    assert_eq!(messages.len(), 2);
    for message in messages {
        assert!(matches!(
            &message,
            Desktop::TerminalAgent(owner, Message::Design(design::Message::ScrollCards { direction: -1, review: true })) if *owner == id
        ));
        no_task(fixture.workspace.update(message));
    }
    assert!(
        fixture
            .workspace
            .panel_review(id, fixture.run(id), None)
            .is_some()
    );
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
    let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
    let mut ui = UserInterface::build(
        design_fixture(fixture, stream, size),
        size,
        Cache::default(),
        &mut renderer,
    );
    ui.operate(&renderer, &mut operation);
    operation
}

fn design_fixture(fixture: &Fixture, stream: u64, size: Size) -> iced::Element<'_, Desktop> {
    let terminal = container(text("Terminal"))
        .id("agent-design-terminal-fixture")
        .width(Length::Fill)
        .height(Length::Fill)
        .into();
    fixture.workspace.decorate_agent_terminal(
        fixture.session,
        stream,
        terminal,
        size,
        Size::new(8.0, 16.0),
        !fixture.workspace.window_settings.agent_animations(),
    )
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
    assert!(fixture.run(id).presentation.drawer_open());
    assert!(!fixture.run(other).presentation.drawer_open());
    assert_eq!(fixture.run(id).epoch, epoch);
    assert!(fixture.run(id).phase == Phase::Thinking);
    assert!(!fixture.run(id).cancel.requested());
}

#[test]
fn top_bar_drawer_lists_exact_steps_without_resizing_the_terminal() {
    use super::pip::{click_log_label, log_label_position};
    use design::log_table::{Message as LogMessage, Row, Surface};
    use engine::usage::{Attempt, AttemptKind, AttemptOutcome};
    use std::time::Duration;

    let mut fixture = Fixture::new(false, false);
    let id = fixture.open_ask_with_source_tab_visible();
    let stream = fixture.workspace.sessions[&fixture.session].stream_generation;
    let run = fixture.run_mut(id);
    run.design.history = History::InlineBlocks;
    run.design.status = Status::TopBar;
    run.goal = "Check two recorded commands".into();
    let epoch = run.next(Phase::Thinking, "Fixture activity");
    let first = run.inline_command("printf first").unwrap();
    run.inline_result(engine::Status::Exited(0), "first output", "");
    run.inline_command("printf second");
    let size = Size::new(780.0, 420.0);
    let closed = terminal_bounds(&fixture, stream, size);
    assert_eq!(closed.width, size.width);
    assert!(closed.height < size.height);

    let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
    let mut ui = UserInterface::build(
        fixture
            .workspace
            .terminal_agents
            .pane_agent_status(fixture.session, stream)
            .unwrap(),
        size,
        Cache::default(),
        &mut renderer,
    );
    let mut messages = click_log_label(&mut ui, &mut renderer, "▾", mouse::Button::Left);
    drop(ui);
    assert!(
        matches!(messages.as_slice(), [Desktop::TerminalAgent(actual, Message::Design(design::Message::ToggleDrawer))] if *actual == id)
    );
    no_task(fixture.workspace.update(messages.remove(0)));
    assert_eq!(terminal_bounds(&fixture, stream, size), closed);
    assert_eq!(fixture.run(id).epoch, epoch);

    let mut ui = UserInterface::build(
        fixture
            .workspace
            .terminal_agents
            .pane_agent_status(fixture.session, stream)
            .unwrap(),
        size,
        Cache::default(),
        &mut renderer,
    );
    let messages = click_log_label(&mut ui, &mut renderer, "printf first", mouse::Button::Right);
    assert!(
        matches!(messages.as_slice(), [Desktop::TerminalAgent(actual,
        Message::Design(design::Message::LogTable(LogMessage::OpenMenu(Surface::StatusStepList,
            Some(Row::Step(step)), _))))] if *actual == id && *step == first)
    );
    drop(ui);
    no_task(fixture.send(id, Message::Design(design::Message::ToggleDrawer)));
    let run = fixture.run_mut(id);
    run.design.history = History::Timeline;
    assert!(run.presentation.model_usage.accept(
        epoch,
        &Attempt {
            kind: AttemptKind::Initial,
            outcome: AttemptOutcome::Completed,
            elapsed: Duration::from_millis(10),
            input_tokens: Some(12),
            output_tokens: Some(8),
            total_tokens: Some(20),
        },
    ));

    let split = Size::new(720.0, 420.0);
    let closed = terminal_bounds(&fixture, stream, split);
    // The 240-pixel timeline and its 3-pixel divider sit beside the terminal.
    assert_eq!(closed.width, split.width - 240.0 - 3.0);
    let mut ui = UserInterface::build(
        design_fixture(&fixture, stream, split),
        split,
        Cache::default(),
        &mut renderer,
    );
    let details = log_label_position(&mut ui, &renderer, "▾");
    assert!(details.x < closed.x + closed.width, "{details:?}");
    let messages = click_log_label(&mut ui, &mut renderer, "▾", mouse::Button::Left);
    drop(ui);
    assert!(
        matches!(messages.as_slice(), [Desktop::TerminalAgent(actual, Message::Design(design::Message::ToggleDrawer))] if *actual == id)
    );
    for message in messages {
        no_task(fixture.workspace.update(message));
    }
    assert!(fixture.run(id).presentation.drawer_open());
    assert_eq!(terminal_bounds(&fixture, stream, split), closed);
}

#[test]
fn hud_opens_typed_steps_above_the_card_inside_a_terminal_with_a_side_dock() {
    use super::pip::{click_log_label, log_label_position};
    use design::log_table::{Message as LogMessage, Row, Surface};

    let mut fixture = Fixture::new(false, false);
    let id = fixture.open_ask_with_source_tab_visible();
    let stream = fixture.workspace.sessions[&fixture.session].stream_generation;
    let run = fixture.run_mut(id);
    run.design.history = History::Timeline;
    run.design.status = Status::Hud;
    run.goal = "Inspect the fixture".into();
    let epoch = run.next(Phase::Thinking, "Fixture activity");
    let first = run.inline_command("printf first").unwrap();
    run.inline_result(engine::Status::Exited(0), "first output", "");
    run.inline_command("printf second");
    let size = Size::new(900.0, 440.0);
    let terminal = terminal_bounds(&fixture, stream, size);
    assert!(terminal.width < size.width);
    assert_eq!(terminal.height, size.height);

    let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
    let mut ui = UserInterface::build(
        design_fixture(&fixture, stream, size),
        size,
        Cache::default(),
        &mut renderer,
    );
    let card = log_label_position(&mut ui, &renderer, "◆ Agent");
    assert!(terminal.contains(card));
    let mut messages = click_log_label(&mut ui, &mut renderer, "Details", mouse::Button::Left);
    drop(ui);
    assert!(
        matches!(messages.as_slice(), [Desktop::TerminalAgent(actual,
        Message::Design(design::Message::ToggleDrawer))] if *actual == id)
    );
    no_task(fixture.workspace.update(messages.remove(0)));
    assert_eq!(terminal_bounds(&fixture, stream, size), terminal);
    assert_eq!(fixture.run(id).epoch, epoch);

    let mut ui = UserInterface::build(
        design_fixture(&fixture, stream, size),
        size,
        Cache::default(),
        &mut renderer,
    );
    assert_eq!(log_label_position(&mut ui, &renderer, "◆ Agent"), card);
    assert!(log_label_position(&mut ui, &renderer, "printf first").y < card.y);
    let messages = click_log_label(&mut ui, &mut renderer, "printf first", mouse::Button::Right);
    assert!(
        matches!(messages.as_slice(), [Desktop::TerminalAgent(actual,
        Message::Design(design::Message::LogTable(LogMessage::OpenMenu(Surface::StatusStepList,
            Some(Row::Step(step)), _))))] if *actual == id && *step == first)
    );
}

#[test]
fn orbit_stays_inside_the_terminal_when_its_steps_open_beside_docked_history() {
    use std::time::{Duration, Instant};

    use super::pip::{click_log_label, log_label_position};
    use design::log_table::{Message as LogMessage, Row, Surface};

    let mut fixture = Fixture::new(false, false);
    fixture.workspace.clipboard.policy.internal = true;
    let id = fixture.open_ask_with_source_tab_visible();
    let stream = fixture.workspace.sessions[&fixture.session].stream_generation;
    let run = fixture.run_mut(id);
    run.design.status = Status::Orbit;
    run.goal = "Inspect the fixture".into();
    let epoch = run.next(Phase::Thinking, "Fixture activity");
    let first = run.inline_command("printf first").unwrap();
    run.inline_result(engine::Status::Exited(0), "first output", "");
    run.inline_command("printf second").unwrap();

    let size = Size::new(900.0, 440.0);
    let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
    for history in [History::Timeline, History::LogTable] {
        let reduced_motion = history == History::LogTable;
        fixture
            .workspace
            .window_settings
            .ai_agent_design_reduced_animations = Some(reduced_motion);
        fixture.run_mut(id).design.history = history;
        let terminal = terminal_bounds(&fixture, stream, size);
        assert!(terminal.width < size.width || terminal.height < size.height);
        let mut ui = UserInterface::build(
            design_fixture(&fixture, stream, size),
            size,
            Cache::default(),
            &mut renderer,
        );
        let ring = log_label_position(&mut ui, &renderer, "1/2");
        assert!(terminal.contains(ring));
        assert!(ring.y > terminal.y + terminal.height - 72.0);
        let mut messages = click_log_label(&mut ui, &mut renderer, "1/2", mouse::Button::Left);
        let cache = ui.into_cache();
        assert!(
            matches!(messages.as_slice(), [Desktop::TerminalAgent(actual,
            Message::Design(design::Message::ToggleDrawer))] if *actual == id)
        );
        no_task(fixture.workspace.update(messages.remove(0)));
        assert!(fixture.run(id).presentation.drawer_open());
        assert_eq!(fixture.run(id).epoch, epoch);
        assert_eq!(terminal_bounds(&fixture, stream, size), terminal);

        let mut ui = UserInterface::build(
            design_fixture(&fixture, stream, size),
            size,
            cache,
            &mut renderer,
        );
        if !reduced_motion {
            let now = Instant::now();
            for at in [now, now + Duration::from_millis(340)] {
                let _ = ui.update(
                    &[Event::Window(window::Event::RedrawRequested(at))],
                    mouse::Cursor::Unavailable,
                    &mut renderer,
                    &mut clipboard::Null,
                    &mut Vec::new(),
                );
            }
        }
        assert_eq!(log_label_position(&mut ui, &renderer, "1/2"), ring);
        let step = log_label_position(&mut ui, &renderer, "printf first");
        assert!(
            terminal.contains(step),
            "expected the step in the Orbit list"
        );
        let mut messages =
            click_log_label(&mut ui, &mut renderer, "printf first", mouse::Button::Right);
        let cache = ui.into_cache();
        assert!(matches!(
            messages.as_slice(),
            [Desktop::TerminalAgent(actual, Message::Design(design::Message::LogTable(
                LogMessage::OpenMenu(Surface::StatusStepList, Some(Row::Step(step)), _)
            )))] if *actual == id && *step == first
        ));
        no_task(fixture.workspace.update(messages.remove(0)));
        let mut ui = UserInterface::build(
            design_fixture(&fixture, stream, size),
            size,
            cache,
            &mut renderer,
        );
        let mut messages =
            click_log_label(&mut ui, &mut renderer, "Copy output", mouse::Button::Left);
        drop(ui);
        assert!(matches!(
            messages.as_slice(),
            [Desktop::TerminalAgent(actual, Message::DesignAction(action))]
                if *actual == id && action.label() == "Copy output"
        ));
        drop(fixture.workspace.update(messages.remove(0)));
        assert_eq!(
            fixture.workspace.clipboard.buffer.get().as_deref(),
            Some("first output")
        );
        fixture
            .run_mut(id)
            .presentation
            .update(design::Message::ToggleDrawer, 0);
    }
}

#[test]
fn hiding_the_hud_closes_its_step_drawer_before_reopening() {
    use std::time::{Duration, Instant};

    let mut fixture = Fixture::new(false, false);
    let id = fixture.open_ask_with_source_tab_visible();
    let owner = fixture.owner;
    let stream = fixture.workspace.sessions[&fixture.session].stream_generation;
    let size = Size::new(900.0, 440.0);
    fixture
        .workspace
        .window_settings
        .ai_agent_design_auto_hide_seconds = Some(1);
    let run = fixture.run_mut(id);
    run.design.status = Status::Hud;
    run.presentation.update(design::Message::ToggleDrawer, 0);
    assert!(fixture.run(id).presentation.drawer_open());

    fixture.workspace.toggle_agent_status(owner);
    assert!(!fixture.run(id).presentation.drawer_open());
    fixture.workspace.toggle_agent_status(owner);
    assert!(!fixture.run(id).presentation.drawer_open());

    let run = fixture.run_mut(id);
    run.presentation.update(design::Message::ToggleDrawer, 0);
    run.presentation
        .finished(Instant::now() - Duration::from_secs(2));
    let terminal = terminal_bounds(&fixture, stream, size);
    assert!(!fixture.run(id).presentation.drawer_open());
    fixture.workspace.toggle_agent_status(owner);
    assert_eq!(terminal_bounds(&fixture, stream, size), terminal);
    assert!(!fixture.run(id).presentation.drawer_open());
}

#[test]
fn status_run_menus_copy_the_displayed_run_and_stay_open_past_auto_hide() {
    use super::pip::click_log_label;
    use design::log_table::{Message as LogMessage, Surface};
    use std::time::{Duration, Instant};

    let mut fixture = Fixture::new(false, false);
    fixture.workspace.clipboard.policy.internal = true;
    fixture.workspace.clipboard.policy.ai_chat_allowed = false;
    fixture
        .workspace
        .window_settings
        .ai_agent_design_auto_hide_seconds = Some(1);
    let earlier = fixture.open_ask_with_source_tab_visible();
    let current = fixture.open_ask_with_source_tab_visible();
    for (id, order, title, answer) in [
        (earlier, 1, "Earlier check", "Earlier answer"),
        (current, 2, "Current check", "Visible answer"),
    ] {
        let run = fixture.run_mut(id);
        run.goal = title.into();
        run.started_order = order;
        run.chat_handoff_id = Some(title.into());
        run.design.history = History::InlineBlocks;
        run.next(Phase::Thinking, "Working");
        run.inline_decision(&engine::Action::Done, "Finished", answer);
        run.next(Phase::Finished, "Finished");
    }
    fixture.workspace.revalidate_agent_chat_handoffs();
    let steps = fixture
        .run(current)
        .status_transcript()
        .unwrap()
        .0
        .steps
        .len();
    let toast_badge = format!("Agent · Completed · {steps} steps");
    let size = Size::new(790.0, 520.0);
    let stream = fixture.workspace.sessions[&fixture.session].stream_generation;
    let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
    for (status, label) in [
        (Status::Pill, "Agent · Completed"),
        (Status::Spotlight, "Completed"),
        (Status::Orbit, "Completed"),
        (Status::Minimap, "Completed"),
        (Status::Toasts, toast_badge.as_str()),
    ] {
        let run = fixture.run_mut(current);
        run.design.status = status;
        run.presentation.finished(Instant::now());
        let mut ui = UserInterface::build(
            design_fixture(&fixture, stream, size),
            size,
            Cache::default(),
            &mut renderer,
        );
        let mut events = if status == Status::Minimap {
            let terminal = terminal_bounds(&fixture, stream, size);
            press_design_at(
                &mut ui,
                &mut renderer,
                Point::new(terminal.x + terminal.width + 1.0, terminal.y + 80.0),
                mouse::Button::Right,
            )
        } else {
            click_log_label(&mut ui, &mut renderer, label, mouse::Button::Right)
        };
        let cache = ui.into_cache();
        assert!(matches!(
            events.as_slice(),
            [Desktop::TerminalAgent(id, Message::Design(design::Message::LogTable(
                LogMessage::OpenMenu(Surface::StatusRun(surface), None, _)
            )))] if *id == current && *surface == status
        ));
        no_task(fixture.workspace.update(events.remove(0)));
        fixture
            .run_mut(current)
            .presentation
            .finished(Instant::now() - Duration::from_secs(3));

        let mut ui = UserInterface::build(
            design_fixture(&fixture, stream, size),
            size,
            cache,
            &mut renderer,
        );
        assert!(
            click_log_label(
                &mut ui,
                &mut renderer,
                "Continue in AI Chat",
                mouse::Button::Left
            )
            .is_empty()
        );
        let mut events =
            click_log_label(&mut ui, &mut renderer, "Copy answer", mouse::Button::Left);
        drop(ui);
        assert!(matches!(
            events.as_slice(),
            [Desktop::TerminalAgent(id, Message::DesignAction(action))]
                if *id == current && action.label() == "Copy answer"
        ));
        drop(fixture.workspace.update(events.remove(0)));
        assert_eq!(
            fixture.workspace.clipboard.buffer.get().as_deref(),
            Some("Visible answer")
        );
    }

    fixture
        .run_mut(current)
        .presentation
        .finished(Instant::now());
    for (label, button) in [
        (toast_badge.as_str(), mouse::Button::Right),
        ("Earlier runs ›", mouse::Button::Left),
        ("Finished · Earlier check", mouse::Button::Left),
    ] {
        let mut ui = UserInterface::build(
            design_fixture(&fixture, stream, size),
            size,
            Cache::default(),
            &mut renderer,
        );
        let mut events = click_log_label(&mut ui, &mut renderer, label, button);
        drop(ui);
        assert_eq!(events.len(), 1);
        drop(fixture.workspace.update(events.remove(0)));
    }
    assert_eq!(
        fixture
            .workspace
            .terminal_agents
            .agent_activity_for_stream(fixture.session, stream)
            .unwrap()
            .0,
        earlier,
    );

    fixture.run_mut(earlier).design.arrangement = Arrangement::Adaptive;
    let narrow = Size::new(480.0, 224.0);
    let open_adaptive_pill = |fixture: &Fixture, renderer: &mut Renderer| {
        let mut ui = UserInterface::build(
            design_fixture(fixture, stream, narrow),
            narrow,
            Cache::default(),
            renderer,
        );
        let events = click_log_label(&mut ui, renderer, "Completed", mouse::Button::Right);
        assert!(matches!(
            events.as_slice(),
            [Desktop::TerminalAgent(id, Message::Design(design::Message::LogTable(
                LogMessage::OpenMenu(Surface::StatusRun(Status::Spotlight), None, _)
            )))] if *id == earlier
        ));
        events.into_iter().next().unwrap()
    };
    let open = open_adaptive_pill(&fixture, &mut renderer);
    no_task(fixture.workspace.update(open));
    let grid = fixture.workspace.sessions[&fixture.session].grid;
    no_task(fixture.workspace.update(Desktop::PaneResized(
        fixture.session,
        GridSize {
            columns: grid.columns + 1,
            ..grid
        },
    )));
    drop(open_adaptive_pill(&fixture, &mut renderer));
}

#[test]
fn minimap_step_context_takes_priority_over_the_rail_run_menu() {
    use design::log_table::{Message as LogMessage, Row, Surface};
    use std::time::{Duration, Instant};
    let mut fixture = Fixture::new(false, false);
    fixture.workspace.clipboard.policy.internal = true;
    fixture
        .workspace
        .window_settings
        .ai_agent_design_auto_hide_seconds = Some(1);
    let id = fixture.open_ask_with_source_tab_visible();
    let run = fixture.run_mut(id);
    run.design.status = Status::Minimap;
    run.design.history = History::InlineBlocks;
    run.goal = "Inspect fixture output".into();
    run.next(Phase::Thinking, "Working");
    let step = run.inline_command("printf fixture").unwrap();
    run.inline_result(engine::Status::Exited(7), "fixture output", "");
    run.inline_decision(&engine::Action::Done, "Finished", "Run answer");
    run.next(Phase::Finished, "Finished");

    let size = Size::new(790.0, 520.0);
    let stream = fixture.workspace.sessions[&fixture.session].stream_generation;
    let terminal = terminal_bounds(&fixture, stream, size);
    assert_eq!(size.width - terminal.width, 14.0);
    let index = fixture
        .run(id)
        .status_transcript()
        .unwrap()
        .0
        .steps
        .iter()
        .position(|entry| entry.id == step)
        .unwrap();
    let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
    let mut ui = UserInterface::build(
        design_fixture(&fixture, stream, size),
        size,
        Cache::default(),
        &mut renderer,
    );
    let point = Point::new(
        terminal.x + terminal.width + 7.0,
        terminal.y + 17.0 + index as f32 * 31.0,
    );
    let mut hover = Vec::new();
    let _ = ui.update(
        &[Event::Mouse(mouse::Event::CursorMoved { position: point })],
        mouse::Cursor::Available(point),
        &mut renderer,
        &mut clipboard::Null,
        &mut hover,
    );
    assert!(
        matches!(hover.as_slice(), [Desktop::TerminalAgent(owner, Message::Design(design::Message::MinimapHover(design::MinimapTarget::Step(actual), true)))] if *owner == id && *actual == step)
    );
    let cache = ui.into_cache();
    no_task(fixture.workspace.update(hover.remove(0)));
    let mut ui = UserInterface::build(
        design_fixture(&fixture, stream, size),
        size,
        cache,
        &mut renderer,
    );
    let output = super::pip::log_label_position(&mut ui, &renderer, "fixture output");
    assert!(output.x < point.x);
    let events = press_design_at(&mut ui, &mut renderer, point, mouse::Button::Right);
    let menus: Vec<_> = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                Desktop::TerminalAgent(_, Message::Design(design::Message::LogTable(_)))
            )
        })
        .collect();
    assert!(
        matches!(menus.as_slice(), [Desktop::TerminalAgent(owner, Message::Design(design::Message::LogTable(
        LogMessage::OpenMenu(Surface::MinimapStep(key), Some(Row::Step(actual)), _)
    )))] if *owner == id && *key == step && *actual == step)
    );
    drop(ui);

    for event in events {
        no_task(fixture.workspace.update(event));
    }
    fixture
        .run_mut(id)
        .presentation
        .finished(Instant::now() - Duration::from_secs(3));
    let mut ui = UserInterface::build(
        design_fixture(&fixture, stream, size),
        size,
        Cache::default(),
        &mut renderer,
    );
    let events =
        super::pip::click_log_label(&mut ui, &mut renderer, "Copy output", mouse::Button::Left);
    drop(ui);
    assert!(events.iter().any(|event| matches!(event, Desktop::TerminalAgent(owner, Message::DesignAction(action)) if *owner == id && action.label() == "Copy output")));
    for event in events {
        drop(fixture.workspace.update(event));
    }
    assert_eq!(
        fixture.workspace.clipboard.buffer.get().as_deref(),
        Some("fixture output")
    );

    fixture.run_mut(id).presentation.finished(Instant::now());
    fixture.run_mut(id).design.history = History::Timeline;
    let with_rail = terminal_bounds(&fixture, stream, size);
    fixture.run_mut(id).design.status = Status::Hidden;
    let without_rail = terminal_bounds(&fixture, stream, size);
    assert_eq!(without_rail.width - with_rail.width, 14.0);
    assert_eq!(without_rail.height, with_rail.height);
}

fn press_design_at(
    ui: &mut UserInterface<'_, Desktop, iced::Theme, Renderer>,
    renderer: &mut Renderer,
    point: Point,
    button: mouse::Button,
) -> Vec<Desktop> {
    let mut events = Vec::new();
    let _ = ui.update(
        &[
            Event::Mouse(mouse::Event::CursorMoved { position: point }),
            Event::Mouse(mouse::Event::ButtonPressed(button)),
            Event::Mouse(mouse::Event::ButtonReleased(button)),
        ],
        mouse::Cursor::Available(point),
        renderer,
        &mut clipboard::Null,
        &mut events,
    );
    events
}
