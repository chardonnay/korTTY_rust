use std::net::SocketAddr;
use std::path::PathBuf;

use kortty_terminal::TerminalProfile;

use crate::connection_store::{Auth, SavedLogin, TunnelConfig, TunnelRule, TunnelTarget};
use crate::session_security::IsolationLevel;
use crate::ssh_dialog::LoginTarget;

use super::*;

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let mut random = [0; 8];
        getrandom::fill(&mut random).unwrap();
        let path = std::env::temp_dir().join(format!(
            "kortty-saved-tunnels-{:016x}",
            u64::from_ne_bytes(random)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn connection(endpoint: LoginTarget, bind: SocketAddr, port: u16) -> Connection {
    Connection {
        id: 0,
        name: "Tunnel test".into(),
        tag: String::new(),
        login: SavedLogin {
            endpoint,
            auth: Auth::Prompt,
            keyboard_interactive: false,
        },
        jump: None,
        emulation: TerminalProfile::Xterm.term_name().into(),
        incognito: false,
        isolation: IsolationLevel::None,
        startup: Default::default(),
        logging: Default::default(),
        terminal_effect: None,
        command_timestamps: None,
        tab_color: None,
        paste_warning: None,
        window_geometry: None,
        terminal: Default::default(),
        tunnels: vec![TunnelConfig {
            enabled: true,
            description: "Local test service".into(),
            rule: TunnelRule {
                bind,
                target: TunnelTarget::Local {
                    host: "127.0.0.1".into(),
                    port,
                },
            },
        }],
    }
}

#[test]
fn unchanged_plan_keeps_the_decision_and_reverted_plan_gets_a_new_review() {
    let directory = TestDirectory::new();
    let mut store = Store::open(directory.0.join("connections.json")).unwrap();
    let id = store
        .save(connection(
            LoginTarget {
                host: "127.0.0.1".into(),
                port: 22,
                username: "test-user".into(),
            },
            "127.0.0.1:9900".parse().unwrap(),
            9901,
        ))
        .unwrap();
    let session = SessionId(9);
    let mut saved = SavedTunnels::new(id, store.path());
    let original = store.get(id).unwrap().clone();
    assert!(saved.change(session, &store, Some(original.plan())));
    let first = saved.review.clone().unwrap();
    saved.phase = Phase::Evaluated;

    let mut changed = original.clone();
    changed.tunnels[0].description = "New label".into();
    store.save(changed.clone()).unwrap();
    assert!(!saved.change(session, &store, Some(changed.plan())));
    assert!(saved.phase == Phase::Evaluated);

    changed.tunnels[0].enabled = false;
    store.save(changed).unwrap();
    assert!(saved.change(session, &store, None));
    assert!(saved.review.is_none());

    store.save(original.clone()).unwrap();
    assert!(saved.change(session, &store, Some(original.plan())));
    let current = saved.review.as_ref().unwrap();
    assert_ne!(current, &first);
    assert!(current.same_plan(&first));
    assert!(saved.phase == Phase::Review);
}

#[cfg(unix)]
mod openssh;
