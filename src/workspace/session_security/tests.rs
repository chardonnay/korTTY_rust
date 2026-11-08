use std::time::{Duration, Instant};

use kortty_terminal::TerminalProfile;

use crate::session::{GridSize, IbmKind, Session, Status};
use crate::ssh_dialog::LoginTarget;

use super::*;
use crate::workspace::{recording_access, ssh_access};

#[test]
fn incognito_layout_omits_private_panes_and_remaps_selected_tab() {
    let mut workspace = Workspace::default();
    let (regular, _) = workspace.open_window(false);
    let (private, _) = workspace.open_window(false);

    let public_pane = workspace.store_session(
        Session::disconnected_ibm(IbmKind::Tn3270e),
        GridSize::default(),
        14,
    );
    let private_pane = workspace.store_session(
        Session::disconnected_ibm(IbmKind::Tn5250),
        GridSize::default(),
        14,
    );
    let second_public = workspace.store_session(
        Session::disconnected_ibm(IbmKind::Tn3270e),
        GridSize::default(),
        14,
    );
    let second_private = workspace.store_session(
        Session::disconnected_ibm(IbmKind::Tn5250),
        GridSize::default(),
        14,
    );
    workspace.add_tab(regular, public_pane);
    workspace.add_tab_with_privacy(regular, private_pane, true);
    workspace.add_tab(regular, second_public);
    workspace.add_tab_with_privacy(private, second_private, true);

    let layout = workspace.capture_layout().unwrap();
    layout.validate().unwrap();
    assert_eq!(layout.windows.len(), 1);
    assert_eq!(layout.windows[0].tabs.len(), 2);
    assert_eq!(layout.windows[0].active, 1);
    assert!(!serde_json::to_string(&layout).unwrap().contains("Tn5250"));
    workspace.windows.get_mut(&regular).unwrap().active = 1;
    assert_eq!(workspace.capture_layout().unwrap().windows[0].active, 0);

    let incognito = workspace.windows[&regular].tabs[1].id();
    let _ = workspace.transfer(regular, Some(private), incognito);
    assert!(workspace.windows[&private].tab().unwrap().incognito);
    assert_eq!(
        workspace.windows[&private].tab().unwrap().focused_session(),
        Some(private_pane)
    );
    assert_eq!(workspace.capture_layout().unwrap().windows.len(), 1);
}

#[test]
fn required_isolation_blocks_in_process_connectors_and_unguarded_shells() {
    let mut workspace = Workspace::default();
    workspace.clipboard.policy.minimum_isolation = IsolationLevel::Sandbox;
    let (window, _) = workspace.open_window(false);
    let _ = workspace.update(Desktop::Ssh(ssh_access::Message::Show(window, None)));
    let ssh_sandbox = crate::ssh_worker::sandbox_available();
    assert_eq!(
        workspace.windows[&window].pending_ssh.is_some(),
        ssh_sandbox
    );
    if ssh_sandbox {
        let _ = workspace.update(Desktop::Ssh(ssh_access::Message::Cancel(window)));
    } else {
        assert!(
            workspace.windows[&window]
                .notice
                .as_deref()
                .unwrap()
                .contains("sandbox")
        );
    }

    let _ = workspace.update(Desktop::ShowIbm(window, IbmKind::Tn3270e, None));
    assert!(workspace.windows[&window].pending_ibm.is_none());
    assert!(
        workspace
            .start_session(window, TerminalProfile::Xterm)
            .is_none()
    );
    assert!(workspace.sessions.is_empty());
}

#[test]
fn an_ended_ssh_worker_is_offline_and_unverified_isolation_takes_priority() {
    let mut workspace = Workspace::default();
    let (window, _) = workspace.open_window(false);
    let mut store_pane = |isolation| {
        let target = ssh_access::SshTarget::from_imported_route(
            LoginTarget {
                host: "server.example".into(),
                port: 22,
                username: "operator".into(),
            },
            None,
            isolation,
        )
        .unwrap();
        workspace
            .store_disconnected_ssh(target, TerminalProfile::Xterm, 14)
            .unwrap()
    };
    let process = store_pane(IsolationLevel::Process);
    let sandbox = store_pane(IsolationLevel::Sandbox);
    workspace.add_tab(window, process);
    let tab = workspace.windows[&window].tab().unwrap();
    assert_eq!(workspace.tab_security(tab).0, "Isolation offline");

    let process = &workspace.sessions[&process];
    let sandbox = &workspace.sessions[&sandbox];
    // Test inputs reproduce the statuses delivered by the stream and sandbox workers.
    let transport_error = Status::Error("SSH worker stopped".into());
    for (status, expected) in [
        (&Status::Starting, "Isolation pending"),
        (&Status::Running, "Own process"),
        (&Status::Exited(None), "Isolation offline"),
        (&transport_error, "Isolation offline"),
    ] {
        assert_eq!(
            Workspace::isolation_markers(false, [(process, status)].into_iter()).0,
            expected
        );
    }
    let (marker, detail) = Workspace::isolation_markers(
        true,
        [(process, &transport_error), (sandbox, &Status::Running)].into_iter(),
    );
    assert_eq!(marker, "Incognito · Isolation unavailable");
    assert!(detail.unwrap().contains("1 offline · 1 unavailable"));
}

#[test]
fn an_incognito_tab_cannot_record_a_real_running_shell() {
    let mut workspace = Workspace::default();
    workspace.clipboard.policy.minimum_isolation = IsolationLevel::Process;
    let (window, _) = workspace.open_window(false);
    let session = workspace
        .start_session(window, TerminalProfile::Xterm)
        .unwrap();
    workspace.add_tab_with_privacy(window, session, true);

    let pane = &workspace.sessions[&session];
    let updates = pane.session.updates();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match &pane.session.frame().status {
            Status::Running => break,
            Status::Starting if Instant::now() < deadline => {
                let _ = updates.try_recv();
                std::thread::sleep(Duration::from_millis(10));
            }
            status => panic!("local PTY failed to start: {status:?}"),
        }
    }
    assert_eq!(pane.isolation_name(&Status::Running), "process");
    assert!(pane.session.local_shell_pid().is_some());
    assert!(!workspace.journal_toggle_enabled(window));
    let _ = workspace.update(Desktop::Recording(recording_access::Message::Toggle(
        window,
    )));
    assert!(matches!(
        workspace.sessions[&session].session.recording_status(),
        crate::session::RecordingStatus::Idle
    ));
    assert!(workspace.capture_layout().unwrap().windows.is_empty());
    workspace.stop_session(session);
}
