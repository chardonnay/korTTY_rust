//! Wire interoperability with a real SSH test peer. The peer's credential
//! checks and command output are deterministic test doubles.
#![cfg(unix)]

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use kortty_ssh::{
    Authentication, Connect, ConnectError, HostKey, KeyboardResponse, SshSession, TrustStore,
    WindowSize,
};

mod support;
use support::TempDirectory;

#[path = "support/keyboard_peer.rs"]
mod keyboard_peer;
use keyboard_peer::{Peer, USER};

#[test]
#[ignore = "requires a test Python with Paramiko 4.0.0 and OpenSSH ssh-keygen"]
fn authenticates_directly_and_after_a_separate_primary_credential() {
    let keys = Keys::new();
    for mode in ["direct", "combined", "password-first", "no-challenge"] {
        let mut peer = keys.peer(mode);
        let endpoint = peer.endpoint();
        let expected_endpoint = endpoint.clone();
        let mut round = 0;
        let handler = move |challenge: &kortty_ssh::KeyboardChallenge| {
            assert_ne!(mode, "no-challenge");
            assert_eq!(challenge.endpoint, expected_endpoint);
            assert_eq!(challenge.username, USER);
            assert!(!challenge.response_timeout.is_zero());
            round += 1;
            let answers: &[&str] = match round {
                1 => {
                    assert_eq!(challenge.name, "Server notice");
                    assert_eq!(challenge.instruction, "Complete the sign-in.");
                    assert!(challenge.prompts.is_empty());
                    &[]
                }
                2 => {
                    assert_eq!(challenge.name, "Two prompts");
                    assert_eq!(challenge.instruction, "Both are needed.");
                    assert_eq!(challenge.prompts.len(), 2);
                    assert_eq!(challenge.prompts[0].text, "Code: ");
                    assert!(!challenge.prompts[0].echo);
                    assert_eq!(challenge.prompts[1].text, "Reason: ");
                    assert!(challenge.prompts[1].echo);
                    &["ä123", ""]
                }
                3 => {
                    assert_eq!(challenge.prompts.len(), 1);
                    assert_eq!(challenge.prompts[0].text, "Confirm: ");
                    assert!(challenge.prompts[0].echo);
                    &["yes"]
                }
                _ => panic!("unexpected test round"),
            };
            Ok(answers
                .iter()
                .map(|answer| KeyboardResponse::new((*answer).into()))
                .collect())
        };
        let authentication = match mode {
            "combined" => Authentication::private_key(fs::read(&keys.client).unwrap())
                .with_keyboard_interactive(handler),
            "password-first" => Authentication::password("password-method-test-value".into())
                .with_keyboard_interactive(handler),
            _ => Authentication::keyboard_interactive(handler),
        };
        let trust = TrustStore::from_pins(vec![(endpoint, keys.host_key.clone())]).unwrap();
        let mut session =
            SshSession::connect_exec(config(&peer, authentication), trust, &[], "fixture-command")
                .unwrap();
        let mut received = String::new();
        session
            .take_reader()
            .unwrap()
            .read_to_string(&mut received)
            .unwrap();
        assert_eq!(received, "keyboard-interactive-ok\n");
        assert_eq!(session.exit_code(), Some(23));
        session.close().unwrap();
        assert_eq!(peer.responses(), if mode == "no-challenge" { 0 } else { 3 });
    }
}

#[test]
#[ignore = "requires a test Python with Paramiko 4.0.0 and OpenSSH ssh-keygen"]
fn refuses_unverified_hosts_and_bounds_invalid_or_cancelled_challenges() {
    let keys = Keys::new();
    for (mode, expected_calls, expected_responses) in [
        ("unknown", 0, 0),
        ("changed", 0, 0),
        ("too-many-prompts", 0, 0),
        ("cancel", 1, 0),
        ("wrong-count", 1, 0),
        ("too-long-answer", 1, 0),
        ("too-many-rounds", 8, 8),
    ] {
        let mut peer = keys.peer(mode);
        let calls = Arc::new(AtomicUsize::new(0));
        let called = Arc::clone(&calls);
        let authentication = Authentication::keyboard_interactive(move |_| {
            called.fetch_add(1, Ordering::SeqCst);
            match mode {
                "cancel" => Err(io::Error::from(io::ErrorKind::Interrupted)),
                "wrong-count" => Ok(vec![KeyboardResponse::new("extra test value".into())]),
                "too-long-answer" => Ok(vec![KeyboardResponse::new("x".repeat(4097))]),
                _ => Ok(Vec::new()),
            }
        });
        let pin = match mode {
            "unknown" => None,
            "changed" => Some(keys.changed_key.clone()),
            _ => Some(keys.host_key.clone()),
        };
        let pins = pin.map(|key| (peer.endpoint(), key));
        let trust = TrustStore::from_pins(pins.into_iter().collect()).unwrap();
        let result = SshSession::connect(config(&peer, authentication), trust);
        match (mode, result) {
            ("unknown", Err(ConnectError::UnknownHostKey { .. })) => {}
            ("changed", Err(ConnectError::ChangedHostKey { .. })) => {}
            ("cancel", Err(ConnectError::Io(error)))
                if error.kind() == io::ErrorKind::Interrupted => {}
            ("wrong-count", Err(ConnectError::Io(error)))
                if error.kind() == io::ErrorKind::InvalidInput => {}
            (
                "too-many-prompts" | "too-many-rounds" | "too-long-answer",
                Err(ConnectError::Protocol(_)),
            ) => {}
            (_, Err(error)) => panic!("unexpected {mode} error: {error}"),
            (_, Ok(_)) => panic!("unexpectedly accepted {mode} login"),
        }
        assert_eq!(calls.load(Ordering::SeqCst), expected_calls, "{mode}");
        assert_eq!(peer.responses(), expected_responses, "{mode}");
    }
}

struct Keys {
    _directory: TempDirectory,
    host: PathBuf,
    host_key: HostKey,
    changed_key: HostKey,
    client: PathBuf,
    client_public: String,
}

impl Keys {
    fn new() -> Self {
        let directory = TempDirectory::new("keyboard-auth");
        let host = directory.0.join("host");
        let client = directory.0.join("client");
        for path in [&host, &client] {
            let output = Command::new("ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", "", "-f"])
                .arg(path)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let host_public = fs::read_to_string(host.with_extension("pub")).unwrap();
        let host_key = HostKey::from_openssh(host_public.trim()).unwrap();
        let client_public = fs::read_to_string(client.with_extension("pub")).unwrap();
        let changed_key = HostKey::from_openssh(client_public.trim()).unwrap();
        let client_public = client_public.split_whitespace().nth(1).unwrap().to_owned();
        Self {
            _directory: directory,
            host,
            host_key,
            changed_key,
            client,
            client_public,
        }
    }

    fn peer(&self, mode: &str) -> Peer {
        keyboard_peer::start(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/keyboard_peer.py"),
            mode,
            &self.host,
            &self.client_public,
        )
    }
}

fn config(peer: &Peer, authentication: Authentication) -> Connect {
    Connect {
        address: peer.address,
        endpoint: peer.endpoint(),
        username: USER.into(),
        authentication,
        term: "xterm".into(),
        size: WindowSize {
            columns: 80,
            rows: 24,
            pixel_width: 0,
            pixel_height: 0,
        },
    }
}
