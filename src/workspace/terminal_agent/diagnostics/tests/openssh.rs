use super::*;

use crate::ssh_connection::Connected;
use crate::ssh_worker::OpenSshFixture;
use crate::terminal_agent::{Status, Target};
use kortty_ssh::{Authentication, Connect, Endpoint, SshSession};
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

#[test]
#[ignore = "requires local OpenSSH and KORTTY_SSH_TEST_EXE pointing to the built app"]
fn live_diagnostic_output_over_direct_and_process_openssh() {
    for isolated in [false, true] {
        let ssh = OpenSshFixture::new();
        let key = ssh.directory().join("client");
        let mut request = ssh.request(key.clone());
        let login = request.target.endpoint.clone();
        let trust = ssh.pinned_trust();
        let server = ssh.serve_with_extra("host", "");
        let mut connection = if isolated {
            request.isolation = crate::session_security::IsolationLevel::Process;
            Connected::Worker(Box::new(
                crate::ssh_worker::dial_test_executable(
                    request,
                    false,
                    &crate::ssh_worker::test_executable(),
                    &trust,
                )
                .unwrap(),
            ))
        } else {
            Connected::Direct(Box::new(
                SshSession::connect(
                    Connect {
                        address: SocketAddr::from((Ipv4Addr::LOCALHOST, login.port)),
                        endpoint: Endpoint::new(&login.host, login.port).unwrap(),
                        username: login.username,
                        authentication: Authentication::private_key(std::fs::read(key).unwrap()),
                        term: "xterm-256color".into(),
                        size: crate::session::ssh_size(Default::default()),
                    },
                    trust,
                )
                .unwrap(),
            ))
        };
        let directory = ssh
            .directory()
            .canonicalize()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let target = Target::Ssh {
            opener: connection.exec_opener().unwrap(),
            directory: directory.clone(),
        };
        let cancel = Cancel::default();
        let mut state = State {
            debug_enabled: true,
            ..State::default()
        };
        let redactor = Redactor::for_journal(
            crate::terminal_logging::Secrets::default().seal().unwrap(),
            Default::default(),
        );
        let (observed, receive) = state.start_output(7, cancel.clone(), redactor);
        let receive = receive.unwrap();
        let worker = std::thread::spawn(move || {
            target.execute(
            "printf 'early fixture\\npassword=fixture-secret\\n'; printf 'early stderr\\n' >&2; sleep 3; printf 'late fixture\\n'",
            &observed, || {},
        )
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !state.live.as_ref().unwrap().text[0].contains("password=***")
            || !state.live.as_ref().unwrap().text[1].contains("early stderr")
        {
            assert!(
                std::time::Instant::now() < deadline,
                "Live SSH diagnostics timed out"
            );
            if let Ok(update) = receive.try_recv() {
                assert!(
                    !worker.is_finished(),
                    "Output must precede command completion"
                );
                assert!(!update.text.contains("fixture-secret"));
                state.update_output(7, &update);
            } else {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        assert!(state.live.as_ref().unwrap().text[0].contains("password=***"));
        assert!(state.live.as_ref().unwrap().text[1].contains("early stderr"));
        cancel.request();
        assert_eq!(worker.join().unwrap().unwrap().status, Status::Cancelled);
        let target = Target::Ssh {
            opener: connection.exec_opener().unwrap(),
            directory,
        };
        let outcome = target
            .execute("printf 'connection survived\\n'", &Cancel::default(), || {})
            .unwrap();
        assert_eq!(outcome.status, Status::Exited(0));
        assert!(outcome.stdout.text().contains("connection survived"));
        connection.close().unwrap();
        let server = server.join().unwrap();
        assert!(String::from_utf8_lossy(&server.stderr).contains("Accepted publickey"));
    }
}
