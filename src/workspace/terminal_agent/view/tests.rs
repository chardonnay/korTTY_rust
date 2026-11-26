use super::*;
use crate::workspace::terminal_agent::{
    LocalDirectory, ReviewedCommand, RunTarget, Target,
    flow::{local_agent_shell, tests::Fixture},
};
use iced::advanced::widget::{Id, Operation, operation};
use iced::{Font, Rectangle, Renderer, Size, Vector};
use iced_runtime::{UserInterface, user_interface::Cache};

#[derive(Default)]
struct Bounds {
    actions: Option<Rectangle>,
    body: Option<(Rectangle, Rectangle)>,
    log_detail_editor: Option<Rectangle>,
    overflowing_table: Option<Rectangle>,
    approval_label: Option<Rectangle>,
}

impl Operation for Bounds {
    fn traverse(&mut self, visit: &mut dyn FnMut(&mut dyn Operation)) {
        visit(self);
    }
    fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
        if id == Some(&Id::new("terminal-agent-actions")) {
            self.actions = Some(bounds);
        }
        if id == Some(&Id::new("agent-log-detail-editor")) {
            self.log_detail_editor = Some(bounds);
        }
    }
    fn text(&mut self, _: Option<&Id>, bounds: Rectangle, content: &str) {
        if content == "Approval" {
            self.approval_label = Some(bounds);
        }
    }
    fn scrollable(
        &mut self,
        id: Option<&Id>,
        bounds: Rectangle,
        content: Rectangle,
        _: Vector,
        _: &mut dyn operation::Scrollable,
    ) {
        if id == Some(&Id::new("terminal-agent-body")) {
            self.body = Some((bounds, content));
        } else if content.width > bounds.width {
            self.overflowing_table = Some(bounds);
        }
    }
}

fn measure(fixture: &Fixture, id: window::Id, size: Size) -> Bounds {
    let state = &fixture.workspace.terminal_agents;
    let run = &state.runs[&id];
    let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
    let mut ui = UserInterface::build(
        run.view(None, true, false, (state, id)),
        size,
        Cache::default(),
        &mut renderer,
    );
    let mut bounds = Bounds::default();
    ui.operate(&renderer, &mut bounds);
    bounds
}

fn assert_scrollable_body_with_footer(fixture: &Fixture, id: window::Id, size: Size) {
    let bounds = measure(fixture, id, size);
    let actions = bounds.actions.expect("the fixed footer");
    assert!(actions.height >= 28.0, "footer was compressed: {actions:?}");
    assert!(
        actions.y >= 0.0 && actions.y + actions.height <= size.height - 10.0,
        "footer escaped viewport: {actions:?} at {size:?}"
    );
    let (body, content) = bounds.body.expect("the scrollable content area");
    assert!(body.height > 0.0 && body.y + body.height <= actions.y);
    if size.height == 400.0 {
        assert!(content.height > body.height, "long content should scroll");
    }
}

#[test]
fn agent_setup_scrolls_long_content_without_clipping_footer_in_any_mode() {
    for mode in [Mode::Run, Mode::Ask, Mode::Plan] {
        for private in [false, true] {
            let mut fixture = Fixture::with_endpoint(false, private, "127.0.0.1:12345");
            let id = fixture.open(mode);
            let run = fixture.run_mut(id);
            run.fonts.activity = 22.0;
            run.fonts.plan = 22.0;
            run.manual_directory = Some("/owned/directory".into());
            run.history.notice =
                "A long retained-history notice for the compact layout. ".repeat(12);
            for size in [
                Size::new(1100.0, 760.0),
                Size::new(800.0, 500.0),
                Size::new(640.0, 400.0),
            ] {
                assert_scrollable_body_with_footer(&fixture, id, size);
            }
        }
    }
}

#[test]
fn triage_choice_scrolls_without_clipping_the_footer() {
    let mut fixture = Fixture::with_endpoint(false, false, "127.0.0.1:12345");
    let id = crate::workspace::terminal_agent::triage::tests::begin_choice(&mut fixture);
    for size in [
        Size::new(1100.0, 760.0),
        Size::new(800.0, 500.0),
        Size::new(640.0, 400.0),
    ] {
        assert_scrollable_body_with_footer(&fixture, id, size);
    }
}

#[test]
fn minimum_probe_review_and_finished_run_keep_the_footer_visible() {
    let mut fixture = Fixture::with_endpoint(false, false, "127.0.0.1:12345");
    let id = fixture.open(Mode::Run);
    let shell = local_agent_shell(&fixture.workspace.sessions[&fixture.session]).unwrap();
    let directory = LocalDirectory::capture(std::env::current_dir().unwrap()).unwrap();
    let run = fixture.run_mut(id);
    run.target = Some(RunTarget::Attached(Target::Local { shell, directory }));
    run.goal = "Review a long fixture task at the minimum dialog size. ".repeat(15);
    run.record("A retained activity entry.".into());
    for phase in [Phase::ProbeApproval, Phase::Finished] {
        fixture.run_mut(id).phase = phase;
        assert_scrollable_body_with_footer(&fixture, id, Size::new(640.0, 400.0));
    }
}

#[test]
fn live_command_review_and_finished_run_can_scroll_at_short_window_sizes() {
    use crate::terminal_agent::{Command, Elevation, Risk};

    let mut fixture = Fixture::with_endpoint(false, false, "127.0.0.1:12345");
    let id = fixture.open(Mode::Run);
    let shell = local_agent_shell(&fixture.workspace.sessions[&fixture.session]).unwrap();
    let directory = LocalDirectory::capture(std::env::current_dir().unwrap()).unwrap();
    let run = fixture.run_mut(id);
    run.target = Some(RunTarget::Attached(Target::Local { shell, directory }));
    run.goal = "Run a desktop activity fixture. ".repeat(20);
    run.history.notice = "A long retained-history notice from the fixture. ".repeat(20);
    run.commands.push_back(ReviewedCommand {
        proposal: Command {
            command: "printf 'fixture output\n'".into(),
            purpose: "Print the fixture output".into(),
            risk: Risk::RequiresConfirmation,
            elevation: Elevation::None,
        },
        sudo: None,
    });
    run.record("Retained output from the environment probe.".into());
    for phase in [Phase::Approval, Phase::Finished] {
        fixture.run_mut(id).phase = phase;
        for size in [
            Size::new(640.0, 400.0),
            Size::new(560.0, 470.0),
            Size::new(800.0, 500.0),
            Size::new(1100.0, 760.0),
        ] {
            assert_scrollable_body_with_footer(&fixture, id, size);
        }
    }
}

#[test]
fn compact_finished_log_table_reserves_space_for_the_selected_output() {
    use crate::terminal_agent::{
        design::History,
        usage::{Attempt, AttemptKind, AttemptOutcome},
    };
    use crate::workspace::terminal_agent::design::{self, log_table};

    let mut fixture = Fixture::with_endpoint(false, false, "127.0.0.1:12345");
    let id = fixture.open(Mode::Run);
    let run = fixture.run_mut(id);
    run.design.history = History::LogTable;
    run.goal = "Display both streams after a completed command".into();
    let epoch = run.next(Phase::Thinking, "Preparing a fixture command");
    run.initialize_inline();
    assert!(run.presentation.model_usage.accept(
        epoch,
        &Attempt {
            kind: AttemptKind::Initial,
            outcome: AttemptOutcome::Completed,
            elapsed: std::time::Duration::from_millis(10),
            input_tokens: Some(11),
            output_tokens: Some(5),
            total_tokens: Some(16),
        },
    ));
    let step = run.inline_command("printf fixture").unwrap();
    run.inline_result(engine::Status::Exited(0), "first stream", "second stream");
    run.inline_decision(&engine::Action::Done, "Done", "Fixture answer");
    run.next(Phase::Finished, "Finished");
    drop(fixture.send(
        id,
        Message::Design(design::Message::LogTable(log_table::Message::Open(
            log_table::Row::Step(step),
        ))),
    ));
    let bounds = measure(&fixture, id, Size::new(640.0, 400.0));
    let editor = bounds
        .log_detail_editor
        .expect("the selected output editor");
    assert!(
        editor.height > 20.0,
        "output editor was compressed: {editor:?}"
    );
}

#[test]
fn minimum_log_table_keeps_a_large_selected_approval_above_the_scrollbar() {
    use crate::terminal_agent::{
        Command, Elevation, Risk,
        design::History,
        usage::{Attempt, AttemptKind, AttemptOutcome},
    };
    use crate::workspace::terminal_agent::design::{self, log_table};

    let mut fixture = Fixture::with_endpoint(false, false, "127.0.0.1:12345");
    let id = fixture.open(Mode::Run);
    let shell = local_agent_shell(&fixture.workspace.sessions[&fixture.session]).unwrap();
    let directory = LocalDirectory::capture(std::env::current_dir().unwrap()).unwrap();
    let run = fixture.run_mut(id);
    run.design.history = History::LogTable;
    run.fonts.activity = 18.0;
    run.target = Some(RunTarget::Attached(Target::Local { shell, directory }));
    run.goal = "Check the minimum Log Table".into();
    run.commands.push_back(ReviewedCommand {
        proposal: Command {
            command: "printf 'fixture output\n'".into(),
            purpose: "Print the fixture output".into(),
            risk: Risk::RequiresConfirmation,
            elevation: Elevation::None,
        },
        sudo: None,
    });
    let revision = run.next(Phase::Approval, "Review the fixture command");
    assert!(run.presentation.model_usage.accept(
        revision,
        &Attempt {
            kind: AttemptKind::Initial,
            outcome: AttemptOutcome::Completed,
            elapsed: std::time::Duration::from_millis(10),
            input_tokens: Some(11),
            output_tokens: Some(5),
            total_tokens: Some(16),
        },
    ));
    run.initialize_inline();
    run.inline_command("fixture preceding row");
    run.inline_result(engine::Status::Exited(0), "fixture output", "");
    run.sync_log_approval();
    drop(fixture.send(
        id,
        Message::Design(design::Message::LogTable(log_table::Message::Select(
            log_table::Row::Approval(revision),
        ))),
    ));
    let bounds = measure(&fixture, id, Size::new(640.0, 400.0));
    let viewport = bounds
        .overflowing_table
        .expect("the Log Table has a horizontal bar");
    let row = bounds.approval_label.expect("the selected Approval row");
    assert!(
        viewport.height >= row.height + 10.0,
        "the row and scrollbar must fit: {viewport:?}, {row:?}"
    );
}
