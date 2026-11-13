use super::*;
use crate::session::{GridSize, TypeAheadRequest};
use kortty_terminal::ScrollbackConfig;

fn view() -> ViewRequest {
    ViewRequest {
        grid: GridSize::default(),
        scroll_anchor: None,
        inline_anchor: None,
        typeahead: TypeAheadRequest::default(),
        prompt_navigation_enabled: true,
        prompt_navigator: Default::default(),
        appearance: Default::default(),
        scrollback: ScrollbackConfig::default(),
        recovery_output_rows: 0,
    }
}

fn record(state: State) -> Update {
    Update {
        id: 7,
        epoch: if state == State::Running { 1 } else { 2 },
        style: Style::Inline,
        animations: false,
        transcript: Transcript {
            goal: "inspect this workspace".into(),
            state,
            final_answer: "inert agent answer".into(),
            ..Transcript::default()
        },
    }
}

#[test]
fn agent_history_projection_preserves_raw_prompt_search_and_reports_its_own_rows() {
    let mut terminal = Terminal::new(80, 18);
    terminal.advance(b"\x1b]133;A\x07$ \x1b]133;B\x07");
    let raw = terminal.snapshot();
    let mut history = History::default();
    assert!(history.apply(&mut terminal, VecDeque::from([record(State::Running)])));
    let search = SearchState::default();
    let frame = history.frame(&mut terminal, Status::Running, &view(), &search);
    assert_eq!(frame.screen, raw);
    assert_eq!(terminal.presentation_rows(), [(7, 0)]);
    let projection = frame.inline.as_ref().unwrap();
    let cursor_row = frame.raw_display_row(raw.cursor.row).unwrap();
    assert!(cursor_row > raw.cursor.row);
    assert_eq!(frame.display_raw_row(cursor_row), Some(raw.cursor.row));
    assert!(projection.visible(7, 1));
    assert!((0..cursor_row).any(|row| projection.line(row).is_some()));
    assert!(
        terminal
            .search("inspect this workspace", false)
            .matches
            .is_empty()
    );
    terminal.advance(b"\x1b[6n");
    assert_eq!(terminal.take_responses(), b"\x1b[1;3R");

    terminal.advance(b"\x1b[?1049h");
    let alternate = history.frame(&mut terminal, Status::Running, &view(), &search);
    assert!(alternate.inline.is_none());
    terminal.advance(b"\x1b[?1049l");
    assert!(
        history
            .frame(&mut terminal, Status::Running, &view(), &search)
            .inline
            .is_some()
    );
}

#[test]
fn agent_history_coexists_with_kitty_but_keeps_raw_mouse_coordinates() {
    let mut terminal = Terminal::new(80, 12);
    terminal.set_pixel_size(9, 18, 720, 216);
    terminal.advance(b"\x1b_Ga=T,f=24,s=1,v=6,c=1,r=3,q=2,C=1;/wAA/wAA/wAA/wAA/wAA/wAA\x1b\\");
    let raw = terminal.snapshot();
    assert_eq!(raw.images.len(), 1);
    assert_eq!(terminal.presentation_blank_tail(), 9);
    let mut history = History::default();
    history.apply(&mut terminal, VecDeque::from([record(State::Running)]));
    let search = SearchState::default();
    let frame = history.frame(&mut terminal, Status::Running, &view(), &search);
    assert!(frame.inline.as_ref().unwrap().visible(7, 1));
    assert_eq!(frame.screen, raw);
    assert!(frame.raw_display_row(2).is_some());
    terminal.advance(b"\x1b[?1000h");
    let reporting = history.frame(&mut terminal, Status::Running, &view(), &search);
    assert!(reporting.screen.mouse_reporting());
    assert!(reporting.inline.is_none());
    assert_eq!(reporting.display_raw_row(2), Some(2));
    assert_eq!(reporting.screen.images, raw.images);
}

#[test]
fn finished_agent_history_scrolls_reflows_and_expires_with_the_anchored_line() {
    let mut terminal = Terminal::new(80, 8);
    terminal.advance(b"$ fixture-prompt");
    let mut history = History::default();
    let search = SearchState::default();
    let mut view = view();
    history.apply(&mut terminal, VecDeque::from([record(State::Finished)]));
    let _ = history.frame(&mut terminal, Status::Running, &view, &search);
    for _ in 0..26 {
        terminal.advance(b"\r\nreal shell output");
    }
    let frame = history.frame(&mut terminal, Status::Running, &view, &search);
    let projection = frame.inline.as_ref().unwrap();
    let first = projection.layout.blocks[0].start;
    let offset = projection.history() - first;
    view.set_inline_anchor(projection.anchor(offset));
    let held = history.frame(&mut terminal, Status::Running, &view, &search);
    assert!(!held.at_live_viewport());
    assert!(held.inline.as_ref().unwrap().line(0).is_some());
    terminal.resize(42, 12);
    let reflowed = history.frame(&mut terminal, Status::Running, &view, &search);
    let projection = reflowed.inline.as_ref().unwrap();
    assert!(
        (0..reflowed.screen.rows)
            .filter_map(|row| projection.line(row))
            .any(|line| line.plain().contains("inert agent answer"))
    );
    assert!(
        terminal
            .search("inert agent answer", false)
            .matches
            .is_empty()
    );

    terminal.set_scrollback_config(ScrollbackConfig {
        lines: 100,
        ..ScrollbackConfig::default()
    });
    for _ in 0..120 {
        terminal.advance(b"\r\nnew raw row");
    }
    assert!(
        history
            .frame(&mut terminal, Status::Running, &view, &search)
            .inline
            .is_none()
    );
    assert!(terminal.presentation_rows().is_empty());
}

#[test]
fn agent_history_keys_require_the_actual_complete_review_footer() {
    let mut update = record(State::Running);
    update.transcript.prompt = Some(inline::Prompt::Approval {
        message: "Exact review · /fixture".into(),
        commands: vec!["printf '%s' fixture".into()],
        allow_always: false,
        keyboard: true,
    });
    assert!(render(&update, 80).approval);
    let narrow = render(&update, 12);
    assert!(!narrow.approval);
    assert!(narrow.lines.iter().all(|line| {
        line.segments
            .iter()
            .all(|segment| segment.role != inline::Role::Key)
    }));
    let Some(inline::Prompt::Approval { commands, .. }) = &mut update.transcript.prompt else {
        unreachable!();
    };
    commands[0].push_str("\nexit");
    assert!(!render(&update, 80).approval);
}

#[test]
fn reconnect_archives_final_records_without_live_prompt_steps_or_focus() {
    let mut old = Terminal::new(80, 12);
    old.advance(b"old prompt");
    let mut source = History::default();
    let mut updates = VecDeque::new();
    for (id, state) in [
        State::Finished,
        State::Failed,
        State::Cancelled,
        State::Running,
        State::Paused,
    ]
    .into_iter()
    .enumerate()
    {
        let mut update = record(state);
        update.id = id as u64 + 1;
        update.style = Style::Focus;
        update.transcript.prompt = Some(inline::Prompt::Password {
            summary: "pending-password".into(),
        });
        update.transcript.steps = vec![
            inline::Step {
                id: 1,
                title: "Retained masked activity".into(),
                ..inline::Step::default()
            },
            inline::Step {
                id: 2,
                title: "Unfinished activity".into(),
                state: StepState::Running,
                ..inline::Step::default()
            },
        ];
        updates.push_back(update);
    }
    source.apply(&mut old, updates);
    let search = SearchState::default();
    let _ = source.frame(&mut old, Status::Running, &view(), &search);
    let mut next = Terminal::new(80, 12);
    next.advance(b"replacement prompt");
    let mut target = History::default();
    target.prepend_reconnect(&mut next, source.capture_reconnect(&old));
    assert_eq!(next.presentation_rows(), [(1, 0), (2, 0), (3, 0)]);
    assert_eq!(target.0.len(), 3);
    for record in &target.0 {
        assert!(record.update.transcript.state.finished());
        assert!(record.update.transcript.prompt.is_none());
        assert_eq!(record.update.transcript.steps.len(), 1);
        assert!(record.since.is_none());
    }
    let mut stale = record(State::Running);
    stale.id = 1;
    target.apply(&mut next, VecDeque::from([stale]));
    let frame = target.frame(&mut next, Status::Running, &view(), &search);
    assert_eq!(target.0[0].update.transcript.state, State::Finished);
    let projection = frame.inline.as_ref().unwrap();
    assert!(
        projection
            .layout
            .blocks
            .iter()
            .all(|block| block.focus.is_none() && !block.approval)
    );
}

#[test]
fn reconnect_transfers_only_anchored_final_agent_history_between_session_actors() {
    use crate::session::{Backend, Event, Session};
    use iced::futures::executor::block_on;

    struct Owned(Session);

    impl Drop for Owned {
        fn drop(&mut self) {
            let _ = self.0.stop().recv_blocking();
        }
    }

    fn wait(
        session: &Session,
        stage: &str,
        matches: impl Fn(&TerminalFrame) -> bool,
    ) -> Arc<TerminalFrame> {
        let deadline = Instant::now() + Duration::from_secs(6);
        loop {
            let frame = session.frame();
            if matches(&frame) {
                return frame;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {stage} in {:?}",
                frame.status
            );
            std::thread::sleep(Duration::from_millis(12));
        }
    }

    fn raw_text(frame: &TerminalFrame) -> String {
        frame
            .screen
            .cells
            .iter()
            .map(|cell| cell.text.as_str())
            .collect()
    }

    fn output(session: &Session, marker: &str) {
        let Backend::Stream { events, actor, .. } = &session.backend else {
            panic!("expected live stream fixture");
        };
        let bytes = format!("\x1b[?1049l\x1b[?1000l\x1b[2J\x1b[H{marker}");
        events
            .send_blocking(Event::Output(bytes.into_bytes(), Instant::now()))
            .unwrap();
        actor.unpark();
        wait(session, marker, |frame| {
            frame.status == Status::Running
                && !frame.screen.alternate_screen
                && !frame.screen.mouse_reporting()
                && raw_text(frame).contains(marker)
        });
    }

    #[cfg(unix)]
    let command = vec![String::from("/bin/cat")];
    #[cfg(windows)]
    let command = {
        let shell = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("System32/cmd.exe");
        [
            shell.to_str().unwrap(),
            "/d",
            "/q",
            "/k",
            "echo",
            "kortty-reconnect-ready",
        ]
        .map(str::to_owned)
        .to_vec()
    };
    let start = || {
        Owned(
            Session::start_local(
                GridSize::default(),
                kortty_terminal::TerminalProfile::Xterm,
                kortty_pty::LaunchOptions {
                    command: Some(command.clone()),
                    ..Default::default()
                },
                true,
                None,
            )
            .unwrap(),
        )
    };
    let old = start();
    let replacement = start();
    for session in [&old.0, &replacement.0] {
        wait(session, "terminal startup", |frame| {
            frame.status == Status::Running
                && (!cfg!(windows) || raw_text(frame).contains("kortty-reconnect-ready"))
        });
    }
    output(&old.0, "physical-reconnect-anchor");
    output(&replacement.0, "physical-reconnect-replacement");
    old.0.publish_agent_history(record(State::Running));
    wait(&old.0, "source anchor", |frame| {
        frame
            .inline
            .as_ref()
            .is_some_and(|inline| inline.visible(7, 1))
    });
    old.0.publish_agent_history(record(State::Finished));
    let mut pending = record(State::Running);
    pending.id = 8;
    pending.transcript.goal = "unarchived running request".into();
    old.0.publish_agent_history(pending);
    let source = wait(&old.0, "source final and pending records", |frame| {
        raw_text(frame).contains("physical-reconnect-anchor")
            && frame.inline.as_ref().is_some_and(|inline| {
                inline
                    .layout
                    .blocks
                    .iter()
                    .any(|block| (block.id, block.epoch) == (7, 2))
                    && inline.layout.blocks.iter().any(|block| block.id == 8)
            })
    });
    let source_projection = source.inline.as_ref().unwrap();
    let source_anchor = source_projection
        .layout
        .blocks
        .iter()
        .find(|block| block.id == 7)
        .unwrap()
        .raw;
    let source_row = u16::try_from(source_anchor - source_projection.raw_start).unwrap();
    assert!(
        source
            .screen
            .row(source_row)
            .unwrap()
            .iter()
            .map(|cell| cell.text.as_str())
            .collect::<String>()
            .contains("physical-reconnect-anchor")
    );
    assert!(block_on(
        old.0.reconnect_history_to(&replacement.0).unwrap()
    ));
    let live = replacement.0.frame();
    assert!(!raw_text(&live).contains("physical-reconnect-anchor"));
    assert!(raw_text(&live).contains("physical-reconnect-replacement"));
    let projection = live.inline.as_ref().unwrap_or_else(|| {
        panic!("replacement projection absent in {:?}, scrollback {}, alternate {}, mouse reporting {}",
            live.status, live.screen.scrollback_lines, live.screen.alternate_screen, live.screen.mouse_reporting());
    });
    let [block] = projection.layout.blocks.as_slice() else {
        panic!("expected only the completed source annotation");
    };
    assert_eq!((block.id, block.epoch), (7, 2));
    replacement
        .0
        .scroll_inline_to(projection.anchor(projection.history() - block.start));
    let archived = wait(&replacement.0, "archived anchor", |frame| {
        frame
            .inline
            .as_ref()
            .is_some_and(|inline| inline.visible(7, 2))
    });
    let projection = archived.inline.as_ref().unwrap();
    assert!(raw_text(&archived).contains("physical-reconnect-anchor"));
    assert!(!raw_text(&archived).contains("inert agent answer"));
    assert!(
        (0..archived.screen.rows)
            .filter_map(|row| projection.line(row))
            .any(|line| line.plain().contains("inert agent answer"))
    );
}

#[test]
fn live_agent_history_update_reaches_an_idle_session_actor() {
    let session = crate::session::Session::start(
        GridSize::default(),
        kortty_terminal::TerminalProfile::Xterm,
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(6);
    while session.frame().status != Status::Running {
        assert!(Instant::now() < deadline, "the fixture shell did not start");
        std::thread::sleep(Duration::from_millis(12));
    }
    session.publish_agent_history(record(State::Running));
    let frame = loop {
        let frame = session.frame();
        if frame
            .inline
            .as_ref()
            .is_some_and(|inline| inline.visible(7, 1))
        {
            break frame;
        }
        assert!(
            Instant::now() < deadline,
            "the idle actor did not publish the history"
        );
        std::thread::sleep(Duration::from_millis(12));
    };
    let raw: String = frame
        .screen
        .cells
        .iter()
        .map(|cell| cell.text.as_str())
        .collect();
    assert!(!raw.contains("inspect this workspace"));
    let _ = session.stop().recv_blocking();
}
