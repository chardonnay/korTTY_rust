use super::*;
use iced::futures::{StreamExt as _, executor::block_on};

#[cfg(unix)]
#[test]
fn live_diagnostic_frames_arrive_before_completion_and_reject_cancelled_or_stale_scope() {
    for changed in 0..4 {
        let mut fixture = Fixture::new(false, false);
        let id = fixture.open(Mode::Run);
        let root = fixture.profile.0.canonicalize().unwrap();
        let shell = local_agent_shell(&fixture.workspace.sessions[&fixture.session]).unwrap();
        let run = fixture.run_mut(id);
        run.manual_directory = Some(root.to_str().unwrap().into());
        run.diagnostics.debug_enabled = true;
        run.target = Some(RunTarget::Attached(Target::Local {
            shell,
            directory: LocalDirectory::capture(root).unwrap(),
        }));
        run.commands.push_back(ReviewedCommand {
            proposal: Command {
                command:
                    "printf 'Early diagnostic line\\n'; sleep 2; printf 'Late diagnostic line\\n'"
                        .into(),
                purpose: "Controlled streaming fixture".into(),
                risk: engine::Risk::RequiresConfirmation,
                elevation: Elevation::None,
            },
            sudo: None,
        });
        let epoch = run.next(Phase::Approval, "Review this fixture command");
        let task = fixture.send(id, Message::Approve(epoch));
        let mut events = iced::futures::stream::SelectAll::new();
        events.push(iced_runtime::task::into_stream(task).unwrap());
        let (epoch, update) = loop {
            let event = block_on(events.next()).expect("A live command event");
            if let iced_runtime::Action::Output(Desktop::TerminalAgent(actual, message)) = event {
                assert_eq!(actual, id);
                assert!(
                    !matches!(message, Message::Command(..)),
                    "A diagnostic must precede completion"
                );
                if let Message::Diagnostic(epoch, update) = message {
                    let _ = fixture.send(id, Message::Diagnostic(epoch, update.clone()));
                    break (epoch, update);
                }
                let next = fixture.send(id, message);
                if let Some(stream) = iced_runtime::task::into_stream(next) {
                    events.push(stream);
                }
            }
        };
        assert!(fixture.run(id).phase == Phase::Running);
        assert!(
            !fixture
                .run(id)
                .activity
                .iter()
                .any(|text| text.contains("Exit status"))
        );
        match changed {
            0 => {
                let _ = fixture.send(id, Message::Cancel);
            }
            1 => {
                let _ = fixture.send(id, Message::Diagnostic(epoch + 1, update.clone()));
            }
            2 => {
                fixture
                    .workspace
                    .sessions
                    .get_mut(&fixture.session)
                    .unwrap()
                    .stream_generation += 1;
            }
            _ => fixture.workspace.clipboard.policy.ai_allowed = false,
        }
        let _ = fixture.send(id, Message::Diagnostic(epoch, update));
        if changed >= 2 {
            assert!(fixture.run(id).phase == Phase::Blocked);
        }
        fixture.run_mut(id).cancel.request();
        while let Some(event) = block_on(events.next()) {
            if let iced_runtime::Action::Output(Desktop::TerminalAgent(actual, message)) = event {
                let next = fixture.send(actual, message);
                if let Some(stream) = iced_runtime::task::into_stream(next) {
                    events.push(stream);
                }
            }
        }
        assert!(!fixture.run(id).phase.working());
    }
}

#[test]
fn diagnostic_defaults_snapshot_each_run_and_ask_disables_runtime_messages() {
    let mut fixture = Fixture::new(false, false);
    let default = fixture.open(Mode::Run);
    assert!(!fixture.run(default).diagnostics.debug_enabled);
    assert!(!fixture.run(default).diagnostics.runtime_enabled);
    let config = fixture.workspace.ai_configuration().unwrap();
    let mut saved = serde_json::to_value(config.as_ref()).unwrap();
    saved["general"]["agent_debug_messages"] = true.into();
    saved["general"]["agent_runtime_messages"] = true.into();
    fixture
        .workspace
        .set_test_ai_configuration(serde_json::from_value(saved).unwrap());
    let enabled = fixture.open(Mode::Run);
    assert!(fixture.run(enabled).diagnostics.debug_enabled);
    assert!(fixture.run(enabled).diagnostics.runtime_enabled);
    assert!(!fixture.run(default).diagnostics.debug_enabled);
    let ask = fixture.open(Mode::Ask);
    assert!(fixture.run(ask).diagnostics.debug_enabled);
    assert!(!fixture.run(ask).diagnostics.runtime_enabled);
    no_task(fixture.send(ask, Message::RuntimeMessages(true)));
    assert!(!fixture.run(ask).diagnostics.runtime_enabled);
    no_task(fixture.send(enabled, Message::DebugMessages(false)));
    assert!(!fixture.run(enabled).diagnostics.debug_enabled);
    fixture
        .run_mut(enabled)
        .next(Phase::ProbeApproval, "Reviewed probe");
    no_task(fixture.send(enabled, Message::DebugMessages(true)));
    no_task(fixture.send(enabled, Message::RuntimeMessages(false)));
    assert!(!fixture.run(enabled).diagnostics.debug_enabled);
    assert!(fixture.run(enabled).diagnostics.runtime_enabled);
}

#[cfg(unix)]
#[test]
fn diagnostic_command_output_is_masked_and_does_not_change_model_evidence() {
    let mut fixture = Fixture::new(false, false);
    let id = fixture.open(Mode::Run);
    let root = fixture.profile.0.canonicalize().unwrap();
    let target = Target::Local {
        shell: Shell::Posix("/bin/sh".into()),
        directory: LocalDirectory::capture(root).unwrap(),
    };
    let outcome = target
        .execute(
            "printf 'password=diagnostic-fixture-secret\\n'",
            &Cancel::default(),
            || {},
        )
        .unwrap();
    let run = fixture.run_mut(id);
    run.goal = "Inspect only this fixture".into();
    run.diagnostics.debug_enabled = true;
    run.record_command("$ harmless fixture command".into(), false);
    run.record_result(&outcome).unwrap();
    assert!(run.diagnostics.view(13.0, 100.0).is_some());
    let evidence = run.decision_input(false, 20000, None).unwrap();
    assert!(!evidence.contains("diagnostic-fixture-secret"));
    assert!(evidence.contains("password=***"));
    let snapshot = run.diagnostics.snapshot(&run.redactor).unwrap();
    run.diagnostics = super::super::super::diagnostics::State::default();
    assert_eq!(evidence, run.decision_input(false, 20000, None).unwrap());
    assert!(run.diagnostics.view(13.0, 100.0).is_none());
    run.diagnostics = snapshot;
    assert!(run.diagnostics.view(13.0, 100.0).is_some());
}
