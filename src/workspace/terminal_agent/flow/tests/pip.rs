//! PiP lifecycle and actual native layout, independent of agent execution.

use iced::Size;

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
            .pip_selected
            .contains_key(&fixture.session)
    );
    assert_ne!(detached, id);
    assert!(
        fixture
            .workspace
            .terminal_agents
            .view(detached, None, false)
            .is_some()
    );
    assert_eq!(
        fixture.workspace.terminal_agents.title(detached),
        Some("kortty · Agent history")
    );
    let close = fixture.workspace.close_terminal_agent(detached).unwrap();
    assert!(close.units() > 0);
    assert!(fixture.run(id).presentation.pip.detached.is_none());
    assert!(!fixture.run(id).cancel.requested());
    assert_eq!(fixture.run(id).epoch, epoch);
    assert!(
        fixture
            .workspace
            .terminal_agents
            .view(detached, None, false)
            .is_none()
    );
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
        fixture.workspace.terminal_agents.pip_selected[&fixture.session],
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
            .pip_selected
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
            .pip_selected
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
            .pip_selected
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
