//! A reviewed saved SSH route binds every hop to an existing host-key pin.

use std::io;

use kortty_ssh::{
    ConnectError, ConnectOptions, Endpoint, HostKey, SshSession, SshTarget, TrustStore,
};

use crate::job_executor::Credentials;
use crate::scheduler_window::ConnectionRef;

use super::super::{Cancel, Outcome, Status, ssh, valid_target_label};

pub(crate) struct Execution {
    pub(crate) review: std::sync::Arc<Review>,
    pub(crate) current: ConnectionRef,
    pub(crate) credentials: Credentials,
    pub(crate) startup: ConnectOptions,
}

impl Execution {
    pub(crate) fn run_guarded(
        self,
        command: &str,
        cancel: &Cancel,
        before_dispatch: impl Fn() -> Result<(), String>,
    ) -> Result<Outcome, String> {
        self.run_checked(
            command,
            cancel,
            crate::ssh_connection::trust_store,
            before_dispatch,
        )
    }

    pub(crate) fn run_checked(
        self,
        command: &str,
        cancel: &Cancel,
        trust_store: impl FnOnce() -> io::Result<TrustStore>,
        before_dispatch: impl Fn() -> Result<(), String>,
    ) -> Result<Outcome, String> {
        self.run_checked_with_start(command, cancel, trust_store, before_dispatch, || {})
    }

    pub(crate) fn run_checked_with_start(
        self,
        command: &str,
        cancel: &Cancel,
        trust_store: impl FnOnce() -> io::Result<TrustStore>,
        before_dispatch: impl Fn() -> Result<(), String>,
        started: impl FnOnce(),
    ) -> Result<Outcome, String> {
        let result = (|| {
            let trust = trust_store().map_err(|_| "Could not open the SSH host-key store.")?;
            let (target, jumps) = self.credentials.into_swarm_route()?;
            let route = Route {
                target,
                jumps,
                startup: self.startup,
            };
            let session = self.review.open(
                &self.current,
                route,
                &trust,
                command,
                cancel,
                before_dispatch,
            )?;
            started();
            ssh::capture_session(session, cancel)
                .map_err(|_| "The SSH command streams are unavailable.".into())
        })();
        if result.is_err() && cancel.requested() {
            return Ok(Outcome {
                status: Status::Cancelled,
                stdout: Default::default(),
                stderr: Default::default(),
            });
        }
        result
    }
}

struct Route {
    target: SshTarget,
    jumps: Vec<SshTarget>,
    startup: ConnectOptions,
}

#[derive(Clone)]
pub(crate) struct Review {
    reference: ConnectionRef,
    directory: String,
    endpoints: Vec<Endpoint>,
    pins: Vec<(Endpoint, HostKey)>,
}

impl Review {
    pub(crate) fn capture(
        reference: ConnectionRef,
        directory: String,
        trust: &TrustStore,
    ) -> io::Result<Self> {
        if reference.id == 0 || !directory.starts_with('/') || !valid_target_label(&directory) {
            return Err(invalid(
                "Choose a saved SSH connection and an absolute remote directory.",
            ));
        }
        let endpoints = route_endpoints(&reference)?;
        let pins = trust.selected_pins(&endpoints)?;
        if pins.len() != endpoints.len() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Verify the host keys for all hops in an interactive connection first.",
            ));
        }
        Ok(Self {
            reference,
            directory,
            endpoints,
            pins,
        })
    }

    pub(crate) fn reference(&self) -> &ConnectionRef {
        &self.reference
    }

    pub(crate) fn directory(&self) -> &str {
        &self.directory
    }

    pub(crate) fn fingerprints(&self) -> impl Iterator<Item = (&Endpoint, String)> {
        self.pins
            .iter()
            .map(|(endpoint, key)| (endpoint, key.fingerprint()))
    }

    pub(crate) fn trust_snapshot(
        &self,
        current: &ConnectionRef,
        trust: &TrustStore,
    ) -> io::Result<TrustStore> {
        if current != &self.reference || trust.selected_pins(&self.endpoints)? != self.pins {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "The saved SSH route or one of its host keys changed. Start a new review.",
            ));
        }
        TrustStore::from_pins(self.pins.clone())
    }

    /// Opens only the approved command, with fresh credentials resolved for the
    /// same saved reference by the caller. No shell runs during review.
    fn open(
        &self,
        current: &ConnectionRef,
        route: Route,
        trust: &TrustStore,
        command: &str,
        cancel: &Cancel,
        before_dispatch: impl Fn() -> Result<(), String>,
    ) -> Result<SshSession, String> {
        let Route {
            target,
            jumps,
            startup,
        } = route;
        if !self.matches_route(&target, &jumps) {
            return Err("The resolved SSH login no longer matches the reviewed route.".into());
        }
        let current_trust = || {
            self.trust_snapshot(current, trust)
                .map_err(|_| "The saved SSH host-key review is no longer current.")
        };
        current_trust()?;
        let command = ssh::remote_command(&self.directory, command)?;
        let entry = jumps.first().unwrap_or(&target);
        let denied = std::cell::RefCell::new(None);
        let cancelled = || {
            if cancel.requested() || denied.borrow().is_some() {
                return true;
            }
            if let Err(error) = before_dispatch() {
                *denied.borrow_mut() = Some(error);
                return true;
            }
            false
        };
        // Startup polls this guard during TCP retries and immediately before SSH exec.
        let tcp = crate::ssh_connection::connect_transport_with_options(
            &entry.endpoint,
            startup,
            cancelled,
        );
        if let Some(error) = denied.take() {
            return Err(error);
        }
        let tcp = tcp.map_err(|_| "Could not connect to the reviewed SSH route.")?;
        let trust = current_trust()?;
        let result = SshSession::connect_exec_preconnected_cancellable(
            tcp, target, jumps, trust, &command, None, cancelled,
        );
        if let Some(error) = denied.into_inner() {
            return Err(error);
        }
        result.map_err(|error| {
            match error {
                ConnectError::UnknownHostKey { .. } | ConnectError::ChangedHostKey { .. } => {
                    "The SSH peer no longer matches the reviewed host-key pins."
                }
                _ => "Could not open the reviewed SSH command channel.",
            }
            .into()
        })
    }

    pub(crate) fn matches_route(&self, target: &SshTarget, jumps: &[SshTarget]) -> bool {
        let same = |login: &crate::ssh_dialog::LoginTarget, target: &SshTarget| {
            login.username == target.username
                && Endpoint::new(&login.host, login.port)
                    .is_ok_and(|endpoint| endpoint == target.endpoint)
        };
        same(&self.reference.target, target)
            && match (&self.reference.jump, jumps) {
                (None, []) => true,
                (Some(login), [jump]) => same(login, jump),
                _ => false,
            }
    }
}

pub(crate) fn route_endpoints(reference: &ConnectionRef) -> io::Result<Vec<Endpoint>> {
    let mut endpoints = reference
        .jump
        .iter()
        .chain(std::iter::once(&reference.target))
        .map(|login| {
            login
                .validate()
                .map_err(|_| invalid("The saved SSH login is incomplete."))?;
            Endpoint::new(&login.host, login.port)
        })
        .collect::<io::Result<Vec<_>>>()?;
    endpoints.sort_unstable();
    endpoints.dedup();
    Ok(endpoints)
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use std::net::{SocketAddr, TcpListener};
    use std::time::{Duration, Instant};

    use kortty_ssh::{Authentication, TrustMode};

    use super::*;
    use crate::ssh_dialog::LoginTarget;

    fn key() -> HostKey {
        HostKey::from_openssh(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBm/RAlphM3+hUG6wWfcO5bIUIaqMLa2ywxcOK1wMWbh",
        )
        .unwrap()
    }

    #[test]
    fn swarm_headless_review_needs_all_pins_and_refuses_changed_login_or_pin() {
        let login = |host: &str| LoginTarget {
            host: host.into(),
            port: 22,
            username: "operator".into(),
        };
        let reference = ConnectionRef {
            id: 3,
            target: login("target.example"),
            jump: Some(login("jump.example")),
        };
        let key = key();
        let target = Endpoint::new("target.example", 22).unwrap();
        let jump = Endpoint::new("jump.example", 22).unwrap();
        let partial = TrustStore::from_pins(vec![(target.clone(), key.clone())]).unwrap();
        assert!(Review::capture(reference.clone(), "/tmp".into(), &partial).is_err());
        let complete = TrustStore::from_pins(vec![(target, key.clone()), (jump, key)]).unwrap();
        let review = Review::capture(reference.clone(), "/tmp".into(), &complete).unwrap();
        assert_eq!(review.directory(), "/tmp");
        assert_eq!(review.reference(), &reference);
        assert_eq!(review.fingerprints().count(), 2);
        assert_eq!(
            review.trust_snapshot(&reference, &complete).unwrap().mode(),
            TrustMode::Strict
        );
        assert!(review.trust_snapshot(&reference, &partial).is_err());
        let target = SshTarget {
            endpoint: Endpoint::new("target.example", 22).unwrap(),
            username: "operator".into(),
            authentication: Authentication::password("fixture".into()),
        };
        let jump = SshTarget {
            endpoint: Endpoint::new("jump.example", 22).unwrap(),
            username: "operator".into(),
            authentication: Authentication::password("fixture".into()),
        };
        assert!(!review.matches_route(&target, &[]));
        assert!(review.matches_route(&target, &[jump]));
        let mut changed = reference;
        changed.target.username = "other".into();
        assert!(review.trust_snapshot(&changed, &complete).is_err());
    }

    #[test]
    fn swarm_headless_changed_or_missing_jump_pin_blocks_before_any_tcp_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let reference = ConnectionRef {
            id: 3,
            target: LoginTarget {
                host: "target.example.test".into(),
                port: 22,
                username: "target".into(),
            },
            jump: Some(LoginTarget {
                host: "127.0.0.1".into(),
                port,
                username: "jump".into(),
            }),
        };
        let target = Endpoint::new("target.example.test", 22).unwrap();
        let jump = Endpoint::new("127.0.0.1", port).unwrap();
        let original =
            TrustStore::from_pins(vec![(target.clone(), key()), (jump.clone(), key())]).unwrap();
        let revoked = TrustStore::from_pins(vec![(target.clone(), key())]).unwrap();
        let changed = HostKey::from_openssh(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBm/RAlphM3+hUG6wWfcO5bIUIaqMLa2ywxcOK1wMWbi",
        )
        .unwrap();
        let rotated =
            TrustStore::from_pins(vec![(target.clone(), key()), (jump.clone(), changed)]).unwrap();
        let review = Review::capture(reference.clone(), "/tmp".into(), &original).unwrap();
        let route = || Route {
            target: SshTarget {
                endpoint: target.clone(),
                username: "target".into(),
                authentication: Authentication::password("fixture".into()),
            },
            jumps: vec![SshTarget {
                endpoint: jump.clone(),
                username: "jump".into(),
                authentication: Authentication::password("fixture".into()),
            }],
            startup: ConnectOptions::QUICK,
        };
        for trust in [&revoked, &rotated] {
            let rejected = review
                .open(
                    &reference,
                    route(),
                    trust,
                    "printf not-launched",
                    &Cancel::default(),
                    || Ok(()),
                )
                .err()
                .expect("reject the revoked or replaced jump pin");
            assert!(rejected.contains("host-key review"));
        }
        let denied = review.open(
            &reference,
            route(),
            &original,
            "printf not-launched",
            &Cancel::default(),
            || Err("Policy revoked before dispatch".into()),
        );
        assert!(matches!(denied, Err(error) if error == "Policy revoked before dispatch"));
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    #[ignore = "requires explicit KORTTY_SSH_EXEC_ADDRESS, USER, PRIVATE_KEY and HOST_KEY fixtures"]
    fn swarm_headless_cancellation_closes_owned_exec_transport() {
        let get = |name| {
            std::env::var(format!("KORTTY_SSH_EXEC_{name}"))
                .expect("set the explicit headless SSH fixture")
        };
        let address: SocketAddr = get("ADDRESS").parse().unwrap();
        let endpoint = Endpoint::new(&address.ip().to_string(), address.port()).unwrap();
        let pin = HostKey::from_openssh(&get("HOST_KEY")).unwrap();
        let username = get("USER");
        let trust = TrustStore::from_pins(vec![(endpoint.clone(), pin)]).unwrap();
        let reference = ConnectionRef {
            id: 1,
            target: LoginTarget {
                host: endpoint.host().into(),
                port: endpoint.port(),
                username: username.clone(),
            },
            jump: None,
        };
        let review = Review::capture(reference.clone(), "/tmp".into(), &trust).unwrap();
        let route = Route {
            target: SshTarget {
                endpoint,
                username,
                authentication: Authentication::private_key(
                    std::fs::read(get("PRIVATE_KEY")).unwrap(),
                ),
            },
            jumps: Vec::new(),
            startup: ConnectOptions::QUICK,
        };
        let cancel = Cancel::default();
        let session = review
            .open(
                &reference,
                route,
                &trust,
                "printf 'start'; sleep 2; printf 'finish'",
                &cancel,
                || Ok(()),
            )
            .unwrap();
        let owner = session.direct_opener();
        let started = Instant::now();
        let outcome = std::thread::scope(|scope| {
            let capture = scope.spawn(|| ssh::capture_session(session, &cancel));
            std::thread::sleep(Duration::from_millis(100));
            cancel.request();
            capture.join()
        });
        // The remote fixture is deliberately finite even when a server keeps
        // a child alive briefly after a non-PTY exec transport closes.
        std::thread::sleep(Duration::from_millis(2200).saturating_sub(started.elapsed()));
        assert_eq!(outcome.unwrap().unwrap().status, Status::Cancelled);
        assert!(owner.is_closed());
    }
}
