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
    let id = fixture.open(Mode::Ask);
    let stream = fixture.workspace.sessions[&fixture.session].stream_generation;
    let size = Size::new(1280.0, 640.0);
    fixture.run_mut(id).design.history = History::Timeline;
    fixture.run_mut(id).design.status = Status::Hidden;
    let fixed = inspect_design(&fixture, stream, size, Histories::default()).0;
    assert_eq!(fixed.len(), 1);

    fixture.run_mut(id).design.arrangement = Arrangement::Adaptive;
    let adaptive = inspect_design(&fixture, stream, size, Histories::default()).0;
    assert_eq!(adaptive, fixed, "the single timeline keeps its full height");

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
    let id = fixture.open(Mode::Ask);
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
    let id = fixture.open(Mode::Ask);
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
    let id = fixture.open(Mode::Ask);
    let other = fixture.open(Mode::Ask);
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
