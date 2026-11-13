use iced::advanced::{
    clipboard,
    widget::{Id, Operation, operation},
};
use iced::{Event, Font, Point, Rectangle, Renderer, Size, keyboard, mouse};
use iced_runtime::{UserInterface, user_interface::Cache};

use crate::session::SessionId;
use crate::terminal_agent::design::Layout as AgentLayout;
use crate::terminal_logging::{Redactor, Secrets};
use crate::workspace::terminal_agent::{ProfileChoice, Purpose, RunSetup, binding, engine};

use super::*;

#[derive(Default)]
struct Controls {
    input: Option<Rectangle>,
    open: Option<Rectangle>,
    dismiss: Option<Rectangle>,
    labels: Vec<String>,
}

impl Operation for Controls {
    fn traverse(&mut self, visit: &mut dyn FnMut(&mut dyn Operation)) {
        visit(self);
    }
    fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
        if id == Some(&Id::new("agent-chat-continue")) {
            self.open = Some(bounds);
        }
        if id == Some(&Id::new("agent-chat-dismiss")) {
            self.dismiss = Some(bounds);
        }
    }
    fn text_input(&mut self, id: Option<&Id>, bounds: Rectangle, _: &mut dyn operation::TextInput) {
        if id == Some(&Id::new("agent-chat-follow-up")) {
            self.input = Some(bounds);
        }
    }
    fn text(&mut self, _: Option<&Id>, _: Rectangle, text: &str) {
        self.labels.push(text.into());
    }
}

fn click(
    ui: &mut UserInterface<'_, Message, iced::Theme, Renderer>,
    renderer: &mut Renderer,
    bounds: Rectangle,
) -> Vec<Message> {
    let cursor = Point::new(
        bounds.x + bounds.width / 2.0,
        bounds.y + bounds.height / 2.0,
    );
    let mut messages = Vec::new();
    ui.update(
        &[
            Event::Mouse(mouse::Event::CursorMoved { position: cursor }),
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
        ],
        mouse::Cursor::Available(cursor),
        renderer,
        &mut clipboard::Null,
        &mut messages,
    );
    messages
}

fn key(key: keyboard::Key, code: keyboard::key::Code, value: Option<&str>) -> Event {
    Event::Keyboard(keyboard::Event::KeyPressed {
        modified_key: key.clone(),
        key,
        physical_key: keyboard::key::Physical::Code(code),
        location: keyboard::Location::Standard,
        modifiers: keyboard::Modifiers::empty(),
        text: value.map(Into::into),
        repeat: false,
    })
}

#[test]
fn native_handoff_click_enter_edit_and_dismiss_carry_only_the_rendered_epoch() {
    let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
    let mut ui = UserInterface::build(
        offer("", 77, true, false, "Run finished"),
        Size::new(600.0, 180.0),
        Cache::default(),
        &mut renderer,
    );
    let mut controls = Controls::default();
    ui.operate(&renderer, &mut controls);
    let messages = click(&mut ui, &mut renderer, controls.open.unwrap());
    assert!(matches!(messages.as_slice(), [Message::ContinueInChat(77)]));
    assert!(click(&mut ui, &mut renderer, controls.input.unwrap()).is_empty());
    let mut messages = Vec::new();
    ui.update(
        &[key(
            keyboard::Key::Character("x".into()),
            keyboard::key::Code::KeyX,
            Some("x"),
        )],
        mouse::Cursor::Unavailable,
        &mut renderer,
        &mut clipboard::Null,
        &mut messages,
    );
    assert!(matches!(messages.as_slice(), [Message::ChatFollowUp(77, text)] if text == "x"));
    messages.clear();
    ui.update(
        &[key(
            keyboard::Key::Named(keyboard::key::Named::Enter),
            keyboard::key::Code::Enter,
            None,
        )],
        mouse::Cursor::Unavailable,
        &mut renderer,
        &mut clipboard::Null,
        &mut messages,
    );
    assert!(matches!(messages.as_slice(), [Message::ContinueInChat(77)]));
    assert!(matches!(
        click(&mut ui, &mut renderer, controls.dismiss.unwrap()).as_slice(),
        [Message::DismissChatHandoff(77)]
    ));
}

#[test]
fn pending_handoff_keeps_the_offer_visible_and_all_composer_actions_inert() {
    for available in [true, false] {
        let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
        let mut ui = UserInterface::build(
            offer("Keep my follow-up", 91, available, true, "Run stopped"),
            Size::new(600.0, 180.0),
            Cache::default(),
            &mut renderer,
        );
        let mut controls = Controls::default();
        ui.operate(&renderer, &mut controls);
        assert!(controls.labels.iter().any(|text| text == "Opening chat…"));
        for bounds in [controls.open, controls.input, controls.dismiss] {
            assert!(click(&mut ui, &mut renderer, bounds.unwrap()).is_empty());
        }
        let mut messages = Vec::new();
        ui.update(
            &[
                key(
                    keyboard::Key::Character("x".into()),
                    keyboard::key::Code::KeyX,
                    Some("x"),
                ),
                key(
                    keyboard::Key::Named(keyboard::key::Named::Enter),
                    keyboard::key::Code::Enter,
                    None,
                ),
            ],
            mouse::Cursor::Unavailable,
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        assert!(messages.is_empty());
    }
}

fn finished_run() -> Run {
    let profile = ProfileChoice {
        id: 1,
        reference: crate::ai_manager::ProfileRef::Numeric(1),
        label: "Fixture".into(),
        reviewed_https: None,
        reviewed_cli: None,
    };
    let mut run = Run::from_setup(RunSetup {
        binding: binding::Binding::Live(binding::Live {
            session: SessionId(7),
            stream: 9,
            private: true,
        }),
        mode: engine::Mode::Ask,
        label: "Fixture terminal".into(),
        context: String::new(),
        manual_directory: None,
        profiles: vec![profile.clone()],
        purpose: Purpose::Agent(profile),
        swarm: false,
        design: AgentLayout::default(),
        redactor: Redactor::for_journal(Secrets::default().seal().unwrap(), Default::default()),
    });
    run.goal = "Inspect service".into();
    run.started_order = 1;
    run.chat_handoff_enabled = true;
    run.chat_handoff_id = Some(crate::private_files::nonce().unwrap());
    run.next(Phase::Thinking, "Working");
    run.next(Phase::Finished, "Finished");
    run
}

#[test]
fn managed_chat_denial_hides_the_expanded_and_compact_handoff() {
    let mut run = finished_run();
    assert!(bar(&run).is_some() && compact(&run).is_some());
    run.chat_handoff_enabled = false;
    assert!(run.can_chat_handoff());
    assert!(bar(&run).is_none() && compact(&run).is_none());
}

#[test]
fn dismissed_offer_remains_reopenable_and_compact_dock_controls_fit_a_small_pane() {
    let mut run = finished_run();
    let identity = run.chat_handoff_id.take();
    assert!(bar(&run).is_none() && compact(&run).is_none());
    run.chat_handoff_id = identity;
    assert!(bar(&run).is_some());
    run.chat_handoff_dismissed = true;
    assert!(bar(&run).is_none());
    assert!(compact(&run).is_some());
    run.chat_handoff_pending = true;
    assert!(
        bar(&run).is_some(),
        "pending handoff remains visible even after reopening a dismissed offer"
    );
    run.chat_handoff_pending = false;
    let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
    let mut ui = UserInterface::build(
        super::super::controls(&run),
        Size::new(300.0, 72.0),
        Cache::default(),
        &mut renderer,
    );
    let mut controls = Controls::default();
    ui.operate(&renderer, &mut controls);
    let open = controls.open.unwrap();
    assert!(open.x >= 0.0 && open.x + open.width <= 300.01);
    assert!(
        open.y + open.height <= 36.0,
        "finished controls need only one compact row"
    );
    assert!(
        !controls
            .labels
            .iter()
            .any(|text| text == "Pause" || text == "Cancel")
    );
    drop(ui);
    run.phase = Phase::Thinking;
    assert!(bar(&run).is_none() && compact(&run).is_none());
    run.phase = Phase::Finished;
    run.closing = true;
    assert!(compact(&run).is_none());
}
