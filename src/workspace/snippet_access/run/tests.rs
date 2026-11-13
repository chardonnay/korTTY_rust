use std::time::{Duration, Instant};

use kortty_terminal::TerminalProfile;

use super::*;
use crate::session::{GridSize, Session};

#[cfg(unix)]
fn save(library: &mut Library, name: &str, language: &str, content: &str) -> Snippet {
    let id = library
        .save(
            None,
            snippets::Input {
                name: name.into(),
                language: language.into(),
                category: String::new(),
                content: content.into(),
            },
        )
        .unwrap();
    library.get(id).unwrap().clone()
}

#[cfg(unix)]
#[test]
fn generated_shell_command_is_one_physical_line_and_preserves_script_bytes() {
    let profile = snippets::tests::Profile::new();
    let mut library = Library::open(profile.path()).unwrap();
    let name = "name ' ; printf unwanted ; #";
    let snippet = save(
        &mut library,
        name,
        "bash",
        "printf '%s\\n' 'Grüße'\r\nprintf '%s\\n' '\\c'",
    );
    let mut command = command_line(&snippet, snippet.content(), false).unwrap();
    assert_eq!(command.pop(), Some(b'\r'));
    assert!(!command.contains(&b'\n'));
    let command = String::from_utf8(command).unwrap();
    let result = std::process::Command::new("/bin/sh")
        .args(["-c", &command])
        .output()
        .unwrap();
    assert!(result.status.success(), "{:?}", result.status);
    assert_eq!(String::from_utf8(result.stdout).unwrap(), "Grüße\n\\c\n");
    assert_eq!(
        String::from_utf8(result.stderr).unwrap(),
        format!("Running snippet: {name}\n")
    );
    assert!(command_line(&snippet, "echo x\x1b[201~", false).is_err());
    assert!(command_line(&snippet, &"'".repeat(4096), false).is_err());
    assert!(command_line(&snippet, "echo x", true).is_err());
}

#[cfg(unix)]
fn output_line(session: &Session, expected: &str) -> bool {
    let frame = session.frame();
    (0..frame.screen.rows).any(|row| {
        frame.screen.row(row).is_some_and(|row| {
            let text: String = row.iter().map(|cell| cell.text.as_str()).collect();
            text.trim() == expected
        })
    })
}

fn wait(mut condition: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(6);
    while !condition() {
        assert!(
            Instant::now() < until,
            "native terminal did not produce the expected state"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(unix)]
#[test]
fn confirmed_variable_runs_only_in_the_named_real_pty_despite_focus_and_broadcast() {
    let profile = snippets::tests::Profile::new();
    let mut library = Library::open(profile.path()).unwrap();
    library
        .save_variable(
            None,
            Variable {
                name: "target".into(),
                value: String::new(),
            },
        )
        .unwrap();
    let snippet = save(
        &mut library,
        "Pinned run",
        "bash",
        "printf '%s%s\\n' 'KORTTY-RUN-' '${target}'\n",
    );
    let snapshot = Arc::new(Snapshot {
        snippet,
        variables: library.variables().to_vec(),
    });
    let mut workspace = Workspace::default();
    workspace.window_settings.paste_warning = paste_protection::WarningMode::Always;
    let (owner, _) = workspace.open_window(false);
    let grid = GridSize::default();
    let first = workspace.store_session(
        Session::start(grid, TerminalProfile::Xterm).unwrap(),
        grid,
        14,
    );
    let second = workspace.store_session(
        Session::start(grid, TerminalProfile::Xterm).unwrap(),
        grid,
        14,
    );
    workspace.add_tab(owner, first);
    let tab = workspace.active_tab_mut(owner).unwrap();
    let (other, _) = tab
        .grid
        .split(pane_grid::Axis::Vertical, tab.focused, second)
        .unwrap();
    tab.broadcast = true;
    wait(|| {
        [first, second]
            .into_iter()
            .all(|id| workspace.sessions[&id].session.frame().status == Status::Running)
    });
    let target = workspace.palette_snippet_target(owner).unwrap();
    assert_eq!(target.session, first);
    assert_eq!(target.pane_number, Some(1));
    workspace.active_tab_mut(owner).unwrap().focused = other;

    // Deliver the background load/check results from a real isolated library.
    let token = workspace.allocate();
    workspace
        .windows
        .get_mut(&owner)
        .unwrap()
        .pending_snippet_run = Some(RunPrompt {
        token,
        id: snapshot.snippet.id(),
        target,
        stage: Stage::Loading,
        trigger: None,
    });
    let _ =
        workspace.update_snippet_run(owner, RunMessage::Loaded(token, Ok(Arc::clone(&snapshot))));
    assert!(!output_line(
        &workspace.sessions[&first].session,
        "KORTTY-RUN-once"
    ));
    let _ = workspace.update_snippet_run(
        owner,
        RunMessage::Value(token, 0, Zeroizing::new("once".into())),
    );
    let _ = workspace.update_snippet_run(owner, RunMessage::Confirm(token));
    let _ = workspace.update_snippet_run(owner, RunMessage::Checked(token, Ok(snapshot)));

    wait(|| output_line(&workspace.sessions[&first].session, "KORTTY-RUN-once"));
    let other_session = &workspace.sessions[&second].session;
    // A later command acknowledged by the other PTY establishes that its
    // input queue had time to process any accidentally broadcast command.
    other_session
        .send(b"printf '%s\\n' 'KORTTY-OTHER-BARRIER'\r".to_vec())
        .unwrap();
    wait(|| output_line(other_session, "KORTTY-OTHER-BARRIER"));
    assert!(!output_line(other_session, "KORTTY-RUN-once"));
    assert_eq!(
        workspace.active_tab_mut(owner).unwrap().focused_session(),
        Some(first)
    );
    assert!(
        Library::open(profile.path()).unwrap().variables()[0]
            .default_value()
            .is_none()
    );
}

#[cfg(unix)]
#[test]
fn resolved_multiline_clipboard_needs_its_own_confirmation_before_pty_input() {
    let profile = snippets::tests::Profile::new();
    let mut library = Library::open(profile.path()).unwrap();
    let snippet = save(
        &mut library,
        "Clipboard fixture",
        "bash",
        "printf '%s\\n' '${clipboard}'",
    );
    let snapshot = Arc::new(Snapshot {
        snippet,
        variables: library.variables().to_vec(),
    });
    let mut workspace = Workspace::default();
    workspace.window_settings.paste_warning = paste_protection::WarningMode::Always;
    let (owner, _) = workspace.open_window(false);
    let grid = GridSize::default();
    let id = workspace.store_session(
        Session::start(grid, TerminalProfile::Xterm).unwrap(),
        grid,
        14,
    );
    workspace.add_tab(owner, id);
    wait(|| workspace.sessions[&id].session.frame().status == Status::Running);
    let target = workspace.palette_snippet_target(owner).unwrap();
    let token = workspace.allocate();
    workspace
        .windows
        .get_mut(&owner)
        .unwrap()
        .pending_snippet_run = Some(RunPrompt {
        token,
        id: snapshot.snippet.id(),
        target,
        stage: Stage::Loading,
        trigger: None,
    });
    let _ =
        workspace.update_snippet_run(owner, RunMessage::Loaded(token, Ok(Arc::clone(&snapshot))));
    let _ = workspace.update_snippet_run(owner, RunMessage::Confirm(token));
    // Deliver a native clipboard task's result without reading the user's clipboard.
    let _ = workspace.update_snippet_run(
        owner,
        RunMessage::Clipboard(
            token,
            Some(Zeroizing::new(
                "KORTTY-CLIP-FIRST\nKORTTY-CLIP-SECOND".into(),
            )),
        ),
    );
    let _ =
        workspace.update_snippet_run(owner, RunMessage::Checked(token, Ok(Arc::clone(&snapshot))));
    let pending = workspace.windows[&owner]
        .pending_snippet_run
        .as_ref()
        .unwrap();
    let Stage::Resolved(resolved, warning) = &pending.stage else {
        panic!("clipboard text bypassed resolved review");
    };
    assert_eq!(warning.explanation(), "Line breaks can submit commands.");
    assert!(
        resolved
            .source
            .contains("KORTTY-CLIP-FIRST\nKORTTY-CLIP-SECOND")
    );
    let confirmation = pending.token;
    assert_ne!(confirmation, token);
    let _ = workspace.update_snippet_run(owner, RunMessage::ConfirmResolved(token));
    let _ = workspace.update_snippet_run(
        owner,
        RunMessage::ResolvedChecked(confirmation, Ok(Arc::clone(&snapshot))),
    );
    let session = &workspace.sessions[&id].session;
    session
        .send(b"printf '%s\\n' 'KORTTY-CLIP-BARRIER'\r".to_vec())
        .unwrap();
    wait(|| output_line(session, "KORTTY-CLIP-BARRIER"));
    assert!(!output_line(session, "KORTTY-CLIP-SECOND"));

    let _ = workspace.update_snippet_run(owner, RunMessage::ConfirmResolved(confirmation));
    let _ = workspace.update_snippet_run(
        owner,
        RunMessage::ResolvedChecked(confirmation, Ok(snapshot)),
    );
    wait(|| output_line(&workspace.sessions[&id].session, "KORTTY-CLIP-SECOND"));
}

#[test]
fn changing_tabs_or_losing_policy_never_retargets_a_palette_row() {
    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    let grid = GridSize::default();
    let first = workspace.store_session(
        Session::start(grid, TerminalProfile::Xterm).unwrap(),
        grid,
        14,
    );
    workspace.add_tab(owner, first);
    wait(|| workspace.sessions[&first].session.frame().status == Status::Running);
    let target = workspace.palette_snippet_target(owner).unwrap();
    assert_eq!(
        workspace
            .palette_snippet_presentation(owner, Some(&target))
            .1,
        None
    );
    workspace.clipboard.policy.problem = Some("invalid managed policy".into());
    assert!(
        workspace
            .palette_snippet_presentation(owner, Some(&target))
            .1
            .unwrap()
            .contains("policy")
    );
    workspace.clipboard.policy.problem = None;
    workspace.windows.get_mut(&owner).unwrap().tabs.clear();
    assert!(
        workspace
            .palette_snippet_presentation(owner, Some(&target))
            .1
            .unwrap()
            .contains("moved or closed")
    );
}

#[test]
fn triggered_plain_snippet_waits_for_explicit_review() {
    let profile = snippets::tests::Profile::new();
    let mut library = Library::open(profile.path()).unwrap();
    let snippet = library
        .save(
            None,
            snippets::Input {
                name: "Triggered plain command".into(),
                language: "bash".into(),
                category: String::new(),
                content: "echo KORTTY-TRIGGER-REVIEW".into(),
            },
        )
        .unwrap();
    let snapshot = Arc::new(Snapshot {
        snippet: library.get(snippet).unwrap().clone(),
        variables: Vec::new(),
    });
    let mut workspace = Workspace::default();
    workspace.window_settings.paste_warning = paste_protection::WarningMode::Off;
    let (owner, _) = workspace.open_window(false);
    let grid = GridSize::default();
    let session = workspace.store_session(
        Session::start(grid, TerminalProfile::Xterm).unwrap(),
        grid,
        14,
    );
    workspace.add_tab(owner, session);
    wait(|| workspace.sessions[&session].session.frame().status == Status::Running);
    let target = workspace.palette_snippet_target(owner).unwrap();
    for revocation in [
        "managed policy",
        "user switch",
        "reconnect",
        "rule revision",
    ] {
        workspace.clipboard.policy.terminal_triggers_allowed = true;
        let trigger = workspace.fixture_highlight_trigger(session, snippet);
        let _ = workspace.invoke_highlight_snippet(owner, Some(target.clone()), trigger);
        let token = workspace.windows[&owner]
            .pending_snippet_run
            .as_ref()
            .unwrap()
            .token;
        let _ = workspace
            .update_snippet_run(owner, RunMessage::Loaded(token, Ok(Arc::clone(&snapshot))));
        let run = workspace.windows[&owner]
            .pending_snippet_run
            .as_ref()
            .unwrap();
        assert!(matches!(&run.stage, Stage::Review(_)) && run.trigger.is_some());

        let _ = workspace.update_snippet_run(owner, RunMessage::Confirm(token));
        assert!(matches!(
            &workspace.windows[&owner]
                .pending_snippet_run
                .as_ref()
                .unwrap()
                .stage,
            Stage::Checking(..)
        ));
        match revocation {
            "managed policy" => workspace.clipboard.policy.terminal_triggers_allowed = false,
            "user switch" => workspace.disable_fixture_highlight_triggers(),
            "reconnect" => {
                workspace
                    .sessions
                    .get_mut(&session)
                    .unwrap()
                    .stream_generation += 1
            }
            "rule revision" => {
                let _ = workspace.fixture_highlight_trigger(session, snippet);
            }
            _ => unreachable!(),
        }
        let _ = workspace
            .update_snippet_run(owner, RunMessage::Checked(token, Ok(Arc::clone(&snapshot))));
        assert!(
            workspace.windows[&owner].pending_snippet_run.is_none(),
            "{revocation}"
        );
        assert!(
            workspace.windows[&owner]
                .notice
                .as_deref()
                .unwrap()
                .contains("trigger"),
            "{revocation}"
        );
    }
}
