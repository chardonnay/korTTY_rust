use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::OwnedFd;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use iced::widget::pane_grid;
use kortty_ssh::{Authentication, Connect, Endpoint, HostKey, SshSession, TrustStore};

use crate::connection_window::{ConnectionWindow, Message as ConnectionMessage, Persisted};
use crate::session::{Session, Status, ssh_size};
use crate::ssh_connection::Connected;
use crate::workspace::{DEFAULT_FONT, initial_grid, ssh_access::SshTarget};

use super::*;
use crate::workspace::tunnel_access::{Field, Mode};

const TIMEOUT: Duration = Duration::from_secs(6);

#[test]
#[ignore = "requires OpenSSH ssh-keygen and sshd and a usable local account"]
fn saves_replace_live_listeners_after_review_and_preserve_manual_forwards() {
    let directory = crate::ssh_test_fixture::TempDirectory::new("saved-tunnel-workspace");
    let ssh = OpenSsh::new(&directory.0);
    let (connected, sshd) = ssh.connect();
    let first_target = TcpListener::bind("127.0.0.1:0").unwrap();
    let next_target = TcpListener::bind("127.0.0.1:0").unwrap();
    let bind = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let manual = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();

    let path = directory.0.join("connections.json");
    let mut store = Store::open(path.clone()).unwrap();
    let id = store
        .save(connection(
            ssh.login.clone(),
            bind,
            first_target.local_addr().unwrap().port(),
        ))
        .unwrap();
    let mut profile = store.get(id).unwrap().clone();
    store.approve(id, profile.plan()).unwrap();

    let mut workspace = Workspace::default();
    let (window, _) = workspace.open_window(false);
    workspace.connection_managers.insert(
        window::Id::unique(),
        crate::workspace::profile_tools::ConnectionManager::new(
            window,
            ConnectionWindow::open(path).unwrap(),
        ),
    );
    let session = store_connected(&mut workspace, connected, &ssh.login, id);
    bind_saved(&mut workspace, session, id, &store);
    workspace.add_tab(window, session);
    let _ = workspace.apply_saved_to(session, &store);
    wait_for(|| workspace.sessions[&session].session.frame().status == Status::Running);
    let _ = workspace.update(WorkspaceMessage::SessionUpdate(session));
    let _ = workspace.update(WorkspaceMessage::Tunnel(Message::Show(window, session)));
    for field in [
        Field::Mode(Mode::Dynamic),
        Field::BindPort(manual.port().to_string()),
    ] {
        let _ = workspace.update(WorkspaceMessage::Tunnel(Message::Edit(window, field)));
    }
    let _ = workspace.update(WorkspaceMessage::Tunnel(Message::Start(window)));
    let _ = workspace.update(WorkspaceMessage::Tunnel(Message::Close(window)));
    assert_eq!(forwarding_of(&workspace, session).active_count(), 2);
    assert!(TcpStream::connect(manual).is_ok());

    let first = forwarding_of(&workspace, session)
        .active
        .keys()
        .copied()
        .collect::<Vec<_>>();
    profile.tunnels[0].description = "Renamed service".into();
    save(&mut workspace, &mut store, &profile);
    assert_eq!(
        first,
        forwarding_of(&workspace, session)
            .active
            .keys()
            .copied()
            .collect::<Vec<_>>()
    );
    assert!(
        workspace
            .pending_tunnel_review(&workspace.windows[&window])
            .is_none()
    );

    profile.tunnels[0].rule.target = TunnelTarget::Local {
        host: "127.0.0.1".into(),
        port: next_target.local_addr().unwrap().port(),
    };
    save(&mut workspace, &mut store, &profile);
    let review = workspace
        .pending_tunnel_review(&workspace.windows[&window])
        .unwrap()
        .clone();
    assert_eq!(forwarding_of(&workspace, session).active_count(), 1);
    let _ = workspace.update(WorkspaceMessage::Tunnel(Message::Review(
        window,
        review.clone(),
        true,
    )));
    let result = review.persist();
    assert!(result.is_ok());
    store = Store::open(store.path().to_path_buf()).unwrap();
    profile.tunnels[0].enabled = false;
    save(&mut workspace, &mut store, &profile);
    profile.tunnels[0].enabled = true;
    save(&mut workspace, &mut store, &profile);
    let current = workspace
        .pending_tunnel_review(&workspace.windows[&window])
        .unwrap()
        .clone();
    assert_ne!(review, current);
    let _ = workspace.update(WorkspaceMessage::Tunnel(Message::Approved(review, result)));
    assert_eq!(
        workspace.pending_tunnel_review(&workspace.windows[&window]),
        Some(&current)
    );
    assert_eq!(forwarding_of(&workspace, session).active_count(), 1);

    let tab = workspace.windows[&window].tab().unwrap().id;
    let (destination, _) = workspace.open_window(false);
    // Exercise tunnel-review rebinding across a forced model move.
    let _ = workspace.transfer(window, Some(destination), tab);
    assert!(
        workspace
            .pending_tunnel_review(&workspace.windows[&window])
            .is_none()
    );
    assert_eq!(
        workspace.pending_tunnel_review(&workspace.windows[&destination]),
        Some(&current)
    );
    let _ = workspace.update(WorkspaceMessage::Tunnel(Message::Review(
        window,
        current.clone(),
        true,
    )));
    assert!(!workspace.tunnel_approval_pending(&current));
    let _ = workspace.update(WorkspaceMessage::Tunnel(Message::Review(
        destination,
        current.clone(),
        true,
    )));
    let result = current.persist();
    assert!(result.is_ok());
    let _ = workspace.update(WorkspaceMessage::Tunnel(Message::Approved(current, result)));
    wait_for(|| {
        let _ = workspace.update(WorkspaceMessage::Tunnel(Message::ListenerChanged(session)));
        forwarding_of(&workspace, session).active_count() == 2
    });
    assert!(TcpStream::connect(manual).is_ok());
    let next = thread::spawn(move || {
        next_target.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match next_target.accept() {
                Ok((mut socket, _)) => return socket.write_all(b"new tunnel target").unwrap(),
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("replacement target did not receive a channel: {error}"),
            }
        }
    });
    let mut client = TcpStream::connect(bind).unwrap();
    client.set_read_timeout(Some(TIMEOUT)).unwrap();
    let mut marker = [0; 17];
    client.read_exact(&mut marker).unwrap();
    assert_eq!(&marker, b"new tunnel target");
    next.join().unwrap();

    store = Store::open(store.path().to_path_buf()).unwrap();
    profile.tunnels[0].enabled = false;
    save(&mut workspace, &mut store, &profile);
    wait_for(|| {
        let _ = workspace.update(WorkspaceMessage::Tunnel(Message::ListenerChanged(session)));
        TcpStream::connect(bind).is_err()
    });
    assert_eq!(forwarding_of(&workspace, session).active_count(), 1);
    assert!(TcpStream::connect(manual).is_ok());
    assert!(
        workspace
            .pending_tunnel_review(&workspace.windows[&destination])
            .is_none()
    );
    workspace.stop_session(session);
    let _ = sshd.join().unwrap();
}

#[test]
#[ignore = "requires OpenSSH ssh-keygen and sshd and a usable local account"]
fn closing_an_ssh_split_moves_approved_local_and_remote_forwards_to_its_live_peer() {
    let directory = crate::ssh_test_fixture::TempDirectory::new("tunnel-split-handoff");
    let ssh = OpenSsh::new(&directory.0);
    let (primary_transport, primary_sshd) = ssh.connect();
    let (foreign_transport, foreign_sshd) = ssh.connect();
    let (intermediate_transport, intermediate_sshd) = ssh.connect();
    let (peer_transport, peer_sshd) = ssh.connect();
    let target = TcpListener::bind("127.0.0.1:0").unwrap();
    let target_port = target.local_addr().unwrap().port();
    let reserved = std::array::from_fn::<_, 3, _>(|_| TcpListener::bind("127.0.0.1:0").unwrap());
    let [local, remote, explicitly_stopped] = reserved
        .each_ref()
        .map(|socket| socket.local_addr().unwrap());
    let mut profile = connection(ssh.login.clone(), local, target_port);
    profile.tunnels.push(TunnelConfig {
        enabled: true,
        description: "Remote service".into(),
        rule: TunnelRule {
            bind: remote,
            target: TunnelTarget::Remote {
                host: "127.0.0.1".into(),
                port: target_port,
            },
        },
    });
    let mut stopped_rule = profile.tunnels[0].clone();
    stopped_rule.rule.bind = explicitly_stopped;
    profile.tunnels.push(stopped_rule);
    let mut store = Store::open(directory.0.join("connections.json")).unwrap();
    let primary_id = store.save(profile.clone()).unwrap();
    profile.name = "Another saved connection".into();
    let foreign_id = store.save(profile).unwrap();
    store
        .approve(primary_id, store.get(primary_id).unwrap().plan())
        .unwrap();

    let mut workspace = Workspace::default();
    let (window, _) = workspace.open_window(false);
    let primary = store_connected(&mut workspace, primary_transport, &ssh.login, primary_id);
    workspace.add_tab(window, primary);
    let primary_pane = workspace.windows[&window].tab().unwrap().focused;
    // Explicit disconnected fixture for tunnel ownership, independent of the
    // interactive Split command which now opens a real SSH login.
    let offline_target = workspace.sessions[&primary].ssh.as_ref().unwrap().clone();
    let offline = workspace
        .store_disconnected_ssh(
            offline_target,
            TerminalProfile::Xterm,
            DEFAULT_FONT,
            Default::default(),
        )
        .unwrap();
    workspace
        .active_tab_mut(window)
        .unwrap()
        .grid
        .split(pane_grid::Axis::Horizontal, primary_pane, offline)
        .unwrap();
    assert_eq!(
        workspace.sessions[&offline].session.frame().status,
        Status::Disconnected
    );

    let foreign = store_connected(&mut workspace, foreign_transport, &ssh.login, foreign_id);
    let intermediate = store_connected(
        &mut workspace,
        intermediate_transport,
        &ssh.login,
        primary_id,
    );
    let peer = store_connected(&mut workspace, peer_transport, &ssh.login, primary_id);
    let tab = workspace.active_tab_mut(window).unwrap();
    tab.grid
        .split(pane_grid::Axis::Horizontal, primary_pane, foreign)
        .unwrap();
    let (intermediate_pane, _) = tab
        .grid
        .split(pane_grid::Axis::Vertical, primary_pane, intermediate)
        .unwrap();
    let (peer_pane, _) = tab
        .grid
        .split(pane_grid::Axis::Vertical, primary_pane, peer)
        .unwrap();
    tab.focused = peer_pane;
    wait_for(|| {
        [primary, foreign, intermediate, peer]
            .into_iter()
            .all(|session| workspace.sessions[&session].session.frame().status == Status::Running)
    });
    let _ = workspace.configure_saved_tunnels(window, peer, primary_id);
    let _ = workspace.configure_saved_tunnels(window, foreign, foreign_id);
    assert!(forwarding_of(&workspace, peer).saved.is_none());
    assert!(forwarding_of(&workspace, foreign).saved.is_none());

    drop(reserved);
    bind_saved(&mut workspace, primary, primary_id, &store);
    let _ = workspace.apply_saved_to(primary, &store);
    wait_for(|| forwarding_of(&workspace, primary).active_count() == 3);
    let service = serve_echo(target, 4);
    round_trip(local);
    round_trip(remote);

    let stop = forwarding_of(&workspace, primary)
        .active
        .iter()
        .find(|(_, entry)| entry.saved == Some(2))
        .map(|(&id, _)| id)
        .unwrap();
    let _ = workspace.update(WorkspaceMessage::Tunnel(Message::Show(window, primary)));
    let _ = workspace.update(WorkspaceMessage::Tunnel(Message::Stop(window, stop)));
    let _ = workspace.update(WorkspaceMessage::Tunnel(Message::Close(window)));

    let _ = workspace.update(WorkspaceMessage::ClosePane(window, primary_pane));
    assert!(!workspace.sessions.contains_key(&primary));
    assert_eq!(
        workspace.windows[&window].tab().unwrap().tunnel_owner,
        Some(intermediate)
    );
    let _ = workspace.update(WorkspaceMessage::ClosePane(window, intermediate_pane));
    assert_eq!(
        workspace.windows[&window].tab().unwrap().tunnel_owner,
        Some(peer)
    );
    assert_eq!(workspace.windows[&window].tab().unwrap().focused, peer_pane);
    wait_for(|| {
        let _ = workspace.update(WorkspaceMessage::Tunnel(Message::ListenerChanged(primary)));
        let _ = workspace.update(WorkspaceMessage::Tunnel(Message::ListenerChanged(
            intermediate,
        )));
        primary_sshd.is_finished()
            && intermediate_sshd.is_finished()
            && forwarding_of(&workspace, peer).active_count() == 2
    });
    let _ = primary_sshd.join().unwrap();
    let _ = intermediate_sshd.join().unwrap();
    round_trip(local);
    round_trip(remote);
    service.join().unwrap();
    assert!(TcpStream::connect(explicitly_stopped).is_err());

    let _ = workspace.update(WorkspaceMessage::ClosePane(window, peer_pane));
    assert!(
        workspace.windows[&window]
            .tab()
            .unwrap()
            .tunnel_owner
            .is_none()
    );
    assert!(forwarding_of(&workspace, foreign).saved.is_none());
    wait_for(|| TcpStream::connect(local).is_err() && TcpStream::connect(remote).is_err());
    workspace.stop_session(foreign);
    workspace.stop_session(offline);
    let _ = peer_sshd.join().unwrap();
    let _ = foreign_sshd.join().unwrap();
}

fn serve_echo(listener: TcpListener, connections: usize) -> thread::JoinHandle<()> {
    listener.set_nonblocking(true).unwrap();
    thread::spawn(move || {
        for _ in 0..connections {
            let mut accepted = None;
            wait_for(|| match listener.accept() {
                Ok((stream, _)) => {
                    accepted = Some(stream);
                    true
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => false,
                Err(error) => panic!("echo fixture could not accept the channel: {error}"),
            });
            let mut stream = accepted.unwrap();
            stream.set_nonblocking(false).unwrap();
            stream.set_read_timeout(Some(TIMEOUT)).unwrap();
            let mut marker = [0; 5];
            stream.read_exact(&mut marker).unwrap();
            stream.write_all(&marker).unwrap();
        }
    })
}

fn round_trip(address: SocketAddr) {
    let mut stream = TcpStream::connect_timeout(&address, TIMEOUT).unwrap();
    stream.set_read_timeout(Some(TIMEOUT)).unwrap();
    stream.write_all(b"hello").unwrap();
    let mut marker = [0; 5];
    stream.read_exact(&mut marker).unwrap();
    assert_eq!(&marker, b"hello");
}

fn store_connected(
    workspace: &mut Workspace,
    transport: Connected,
    login: &LoginTarget,
    id: u64,
) -> SessionId {
    let grid = initial_grid(TerminalProfile::Xterm);
    let (direct, remote) = transport.tunnel_openers().unwrap();
    let session = Session::ssh(Some(transport), grid, TerminalProfile::Xterm, true, None).unwrap();
    let session = workspace.store_session(session, grid, DEFAULT_FONT);
    let pane = workspace.sessions.get_mut(&session).unwrap();
    pane.ssh = Some(
        serde_json::from_value::<SshTarget>(serde_json::json!({
            "host": login.host, "port": login.port, "username": login.username, "saved": id,
        }))
        .unwrap(),
    );
    pane.forwarding = Some(Forwarding::new(direct, remote));
    session
}

fn bind_saved(workspace: &mut Workspace, session: SessionId, id: u64, store: &Store) {
    workspace
        .sessions
        .get_mut(&session)
        .unwrap()
        .forwarding
        .as_mut()
        .unwrap()
        .saved = Some(SavedTunnels::new(id, store.path()));
}

fn save(workspace: &mut Workspace, store: &mut Store, profile: &Connection) {
    let id = store.save(profile.clone()).unwrap();
    let manager = *workspace.connection_managers.keys().next().unwrap();
    let _ = workspace.update(WorkspaceMessage::Connections(
        manager,
        ConnectionMessage::Completed(Ok(Box::new(Persisted::saved(store.clone(), id)))),
    ));
}

fn forwarding_of(workspace: &Workspace, session: SessionId) -> &Forwarding {
    workspace.sessions[&session].forwarding.as_ref().unwrap()
}

fn wait_for(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the native listener state"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

struct OpenSsh<'a> {
    directory: &'a Path,
    listener: TcpListener,
    config: PathBuf,
    login: LoginTarget,
    trust: TrustStore,
}

impl<'a> OpenSsh<'a> {
    fn new(directory: &'a Path) -> Self {
        for name in ["host", "client"] {
            let result = Command::new("ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", "", "-f"])
                .arg(directory.join(name))
                .output()
                .unwrap();
            assert!(result.status.success(), "{result:?}");
        }
        let user = Command::new("id").arg("-un").output().unwrap();
        assert!(user.status.success());
        let username = String::from_utf8(user.stdout).unwrap().trim().to_owned();
        let config = directory.join("sshd.conf");
        fs::write(
            &config,
            format!(
                "HostKey \"{}\"\nAuthorizedKeysFile \"{}\"\nAllowUsers {username}\n\
         PasswordAuthentication no\nKbdInteractiveAuthentication no\nPubkeyAuthentication yes\n\
         UsePAM no\nStrictModes yes\nPermitTTY yes\nAllowTcpForwarding yes\n",
                directory.join("host").display(),
                directory.join("client.pub").display(),
            ),
        )
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let endpoint = Endpoint::new("127.0.0.1", address.port()).unwrap();
        let pin = HostKey::from_openssh(
            fs::read_to_string(directory.join("host.pub"))
                .unwrap()
                .trim(),
        )
        .unwrap();
        let trust = TrustStore::from_pins(vec![(endpoint.clone(), pin)]).unwrap();
        let login = LoginTarget {
            host: "127.0.0.1".into(),
            port: address.port(),
            username,
        };

        Self {
            directory,
            listener,
            config,
            login,
            trust,
        }
    }

    fn connect(&self) -> (Connected, thread::JoinHandle<Output>) {
        let (connected, sshd) = self.connect_with_trust(self.trust.clone());
        (connected.unwrap(), sshd)
    }

    fn connect_with_trust(
        &self,
        trust: TrustStore,
    ) -> (
        Result<Connected, kortty_ssh::ConnectError>,
        thread::JoinHandle<Output>,
    ) {
        let listener = self.listener.try_clone().unwrap();
        let address = listener.local_addr().unwrap();
        let config = self.config.clone();
        let sshd = thread::spawn(move || {
            let input: OwnedFd = listener.accept().unwrap().0.into();
            let output = input.try_clone().unwrap();
            Command::new("/usr/sbin/sshd")
                .args(["-ie", "-f"])
                .arg(config)
                .stdin(Stdio::from(input))
                .stdout(Stdio::from(output))
                .stderr(Stdio::piped())
                .output()
                .unwrap()
        });

        let endpoint = Endpoint::new("127.0.0.1", address.port()).unwrap();
        let connected = SshSession::connect(
            Connect {
                address,
                endpoint,
                username: self.login.username.clone(),
                authentication: Authentication::private_key(
                    fs::read(self.directory.join("client")).unwrap(),
                ),
                term: "xterm".into(),
                size: ssh_size(initial_grid(TerminalProfile::Xterm)),
            },
            trust,
        )
        .map(|session| Connected::Direct(Box::new(session)));

        (connected, sshd)
    }
}

#[test]
#[ignore = "requires OpenSSH ssh-keygen and sshd and a usable local account"]
fn openssh_terminal_logging_finishes_archive_before_session_stop_completes() {
    use crate::terminal_logging::{Edit, Form, Format, Policy, Prepared};

    let directory = crate::ssh_test_fixture::TempDirectory::new("terminal-log-openssh");
    let root = directory.0.canonicalize().unwrap();
    let fixture = OpenSsh::new(&root);
    let (connected, sshd) = fixture.connect();
    let mut form = Form::new(Default::default());
    form.update(Edit::Enabled(true));
    form.update(Edit::Format(Format::Json));
    let prepared = Prepared::new(
        form.config().unwrap(),
        Policy::default(),
        "actual OpenSSH".into(),
        fixture.login.host.clone(),
        fixture.login.username.clone(),
    )
    .with_test_profile(root.clone());
    prepared.secrets().add("real-secret");
    let session = Session::ssh_with_logging(
        Some(connected),
        initial_grid(TerminalProfile::Xterm),
        TerminalProfile::Xterm,
        true,
        None,
        Some(Box::new(prepared)),
        Default::default(),
    )
    .unwrap();
    wait_for(|| session.frame().status == Status::Running);
    session
        .send(b"printf 'transport %s%s\\n' real- secret\n".to_vec())
        .unwrap();
    let folder = root.join("terminal-logs");
    wait_for(|| {
        fs::read_dir(&folder)
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .any(|path| {
                path.extension().and_then(std::ffi::OsStr::to_str) == Some("json")
                    && fs::read_to_string(path).is_ok_and(|raw| raw.contains("transport ***"))
            })
    });
    assert_eq!(session.frame().status, Status::Running);
    let done = session.stop();
    wait_for(|| done.is_closed());
    assert!(
        session.logfile_notice().is_none(),
        "{:?}",
        session.logfile_notice()
    );
    let files: Vec<_> = fs::read_dir(&folder)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert!(
        !files
            .iter()
            .any(|path| path.extension().and_then(std::ffi::OsStr::to_str) == Some("lock"))
    );
    let archived = files
        .iter()
        .find(|path| path.extension().and_then(std::ffi::OsStr::to_str) == Some("gz"))
        .unwrap();
    let mut text = String::new();
    flate2::read::GzDecoder::new(fs::File::open(archived).unwrap())
        .read_to_string(&mut text)
        .unwrap();
    let log: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(
        log["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["line"] == "transport ***")
    );
    assert!(!text.contains("real-secret"));
    let _ = sshd.join().unwrap();
}

#[test]
#[ignore = "requires OpenSSH ssh-keygen and sshd and a usable local account"]
fn openssh_same_server_split_creates_an_independent_ssh_shell() {
    use crate::ssh_dialog::{self, LoginMessage};
    use crate::workspace::ssh_access;

    let directory = crate::ssh_test_fixture::TempDirectory::new("same-server-split");
    let server = OpenSsh::new(&directory.0);
    let (original, original_server) = server.connect();
    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    let grid = initial_grid(TerminalProfile::Xterm);
    let session = Session::ssh(Some(original), grid, TerminalProfile::Xterm, true, None).unwrap();
    let source = workspace.store_session(session, grid, 19);
    let route =
        SshTarget::from_imported_route(server.login.clone(), None, Default::default()).unwrap();
    workspace.sessions.get_mut(&source).unwrap().ssh = Some(route.clone());
    workspace.add_tab(owner, source);
    wait_for(|| workspace.sessions[&source].session.frame().status == Status::Running);

    let _ = workspace.update(WorkspaceMessage::Chrome(
        crate::workspace::chrome::Message::Invoke(
            owner,
            crate::workspace::chrome::Action::SplitRight,
        ),
    ));
    assert_eq!(workspace.windows[&owner].tab().unwrap().grid.len(), 1);
    for input in [
        LoginMessage::Host("ignored.example".into()),
        LoginMessage::Port("1".into()),
        LoginMessage::Username("other".into()),
        LoginMessage::Authentication(ssh_dialog::Method::PrivateKey),
        LoginMessage::KeyFile(directory.0.join("client").to_str().unwrap().into()),
    ] {
        let _ = workspace.update_ssh(ssh_access::Message::Form(
            owner,
            ssh_dialog::Message::Target(input),
        ));
    }
    let _ = workspace.update_ssh(ssh_access::Message::Form(
        owner,
        ssh_dialog::Message::UseJump(true),
    ));
    let dial = workspace.update_ssh(ssh_access::Message::Form(
        owner,
        ssh_dialog::Message::Connect,
    ));
    assert_eq!(dial.units(), 1);

    // Connect the actual pinned loopback server without touching the desktop trust profile,
    // then deliver the normal success event that the desktop dial task would produce.
    let (second, second_server) = server.connect();
    let _ = workspace.receive_ssh_fixture_connection(owner, second);
    let tab = workspace.windows[&owner].tab().unwrap();
    assert_eq!(tab.grid.len(), 2);
    let split = tab.focused_session().unwrap();
    assert_ne!(split, source);
    assert_eq!(workspace.sessions[&split].ssh.as_ref(), Some(&route));
    assert_eq!(workspace.sessions[&split].font, 19);

    let has_marker = |session: SessionId, marker: &str| {
        let frame = workspace.sessions[&session].session.frame();
        frame
            .screen
            .cells
            .iter()
            .map(|cell| cell.text.as_str())
            .collect::<String>()
            .contains(marker)
    };
    for (session, command, marker) in [
        (
            source,
            b"printf '%s%s\\n' 'same-' 'original'\r".as_slice(),
            "same-original",
        ),
        (
            split,
            b"printf '%s%s\\n' 'same-' 'second'\r".as_slice(),
            "same-second",
        ),
    ] {
        wait_for(|| workspace.sessions[&session].session.frame().status == Status::Running);
        workspace.sessions[&session]
            .session
            .send(command.to_vec())
            .unwrap();
        wait_for(|| has_marker(session, marker));
    }
    assert!(!has_marker(source, "same-second"));
    assert!(!has_marker(split, "same-original"));
    workspace.stop_session(source);
    workspace.stop_session(split);
    original_server.join().unwrap();
    second_server.join().unwrap();
}

#[test]
#[ignore = "requires OpenSSH ssh-keygen and sshd and a usable local account"]
fn openssh_reopen_closed_ssh_verifies_the_pin_and_starts_a_fresh_saved_session() {
    use crate::connection_store::{Auth, SavedLogin};
    use crate::workspace::{recently_closed, ssh_access};

    let directory = crate::ssh_test_fixture::TempDirectory::new("reopen-closed-ssh");
    let server = OpenSsh::new(&directory.0);
    let (original, original_server) = server.connect();
    let mut store = Store::open(directory.0.join("connections.json")).unwrap();
    let saved_id = store
        .save(Connection {
            id: 0,
            name: "Local OpenSSH fixture".into(),
            tag: String::new(),
            login: SavedLogin {
                endpoint: server.login.clone(),
                auth: Auth::KeyFile {
                    path: directory.0.join("client"),
                    passphrase: None,
                },
                keyboard_interactive: false,
            },
            jump: None,
            emulation: TerminalProfile::Xterm.term_name().into(),
            tunnels: Vec::new(),
            incognito: false,
            isolation: Default::default(),
            startup: Default::default(),
            logging: Default::default(),
            terminal_effect: None,
            command_timestamps: None,
            tab_color: None,
            paste_warning: None,
            window_geometry: None,
            terminal: Default::default(),
        })
        .unwrap();

    let mut workspace = Workspace::default();
    workspace.clipboard.policy.port_forwarding_allowed = false;
    let (owner, _) = workspace.open_window(false);
    let grid = initial_grid(TerminalProfile::Xterm);
    let session = Session::ssh(Some(original), grid, TerminalProfile::Xterm, true, None).unwrap();
    let source = workspace.store_session(session, grid, DEFAULT_FONT);
    let origin =
        SshTarget::from_imported_route(server.login.clone(), None, Default::default()).unwrap();
    workspace.sessions.get_mut(&source).unwrap().ssh = Some(origin);
    workspace.add_tab(owner, source);
    let tab = workspace.active_tab_mut(owner).unwrap();
    tab.custom_title = Some("Reopen this SSH tab".into());
    let closed_tab = tab.id;
    let contains = |workspace: &Workspace, id: SessionId, marker: &str| {
        workspace.sessions[&id]
            .session
            .frame()
            .screen
            .cells
            .iter()
            .map(|cell| cell.text.as_str())
            .collect::<String>()
            .contains(marker)
    };
    wait_for(|| workspace.sessions[&source].session.frame().status == Status::Running);
    workspace.sessions[&source]
        .session
        .send(b"printf '%s%s\\n' 'old-' 'reopen-output'\r".to_vec())
        .unwrap();
    wait_for(|| contains(&workspace, source, "old-reopen-output"));
    let _ = workspace.update(WorkspaceMessage::CloseTab(owner, closed_tab));
    original_server.join().unwrap();
    let entry = workspace.recently_closed_menu_rows()[0].0;

    let _ = workspace.reopen_closed_tab(Some(owner), Some(entry));
    let dial = workspace.launch_saved_reopen_fixture(owner, &store, saved_id);
    assert_eq!(dial.units(), 1);
    let wrong_key = HostKey::from_openssh(
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAINdamAGCsQq31Uv+08lkBzoO4XLz2qYjJa8CGmj3B1Ea",
    )
    .unwrap();
    let endpoint = Endpoint::new(&server.login.host, server.login.port).unwrap();
    let wrong_trust = TrustStore::from_pins(vec![(endpoint, wrong_key)]).unwrap();
    let (refused, refusing_server) = server.connect_with_trust(wrong_trust);
    assert!(matches!(
        &refused,
        Err(kortty_ssh::ConnectError::ChangedHostKey { .. })
    ));
    let _ = workspace.receive_ssh_fixture_result(owner, refused);
    refusing_server.join().unwrap();
    assert!(workspace.windows[&owner].tabs.is_empty());
    assert!(workspace.has_recently_closed_entries());

    let _ = workspace.update_ssh(ssh_access::Message::Cancel(owner));
    let _ = workspace.update_recently_closed(recently_closed::Message::Advance(owner, entry));
    assert!(workspace.has_recently_closed());
    let _ = workspace.reopen_closed_tab(Some(owner), Some(entry));
    let dial = workspace.launch_saved_reopen_fixture(owner, &store, saved_id);
    assert_eq!(dial.units(), 1);

    // The test supplies the genuine verified OpenSSH result to the same completion
    // event the desktop worker uses, keeping the test pins out of the user's store.
    let (connected, reopened_server) = server.connect();
    let _ = workspace.receive_ssh_fixture_connection(owner, connected);
    let _ = workspace.update_recently_closed(recently_closed::Message::Advance(owner, entry));
    let tab = workspace.windows[&owner].tab().unwrap();
    assert_ne!(tab.id, closed_tab);
    assert_eq!(tab.custom_title.as_deref(), Some("Reopen this SSH tab"));
    let reopened = tab.focused_session().unwrap();
    assert_ne!(reopened, source);
    assert!(!workspace.has_recently_closed_entries());
    assert_eq!(
        workspace.sessions[&reopened]
            .ssh
            .as_ref()
            .unwrap()
            .saved_id(),
        Some(saved_id)
    );
    wait_for(|| workspace.sessions[&reopened].session.frame().status == Status::Running);
    workspace.sessions[&reopened]
        .session
        .send(b"printf '%s%s\\n' 'fresh-' 'reopen-output'\r".to_vec())
        .unwrap();
    wait_for(|| contains(&workspace, reopened, "fresh-reopen-output"));
    assert!(!contains(&workspace, reopened, "old-reopen-output"));
    workspace.stop_session(reopened);
    reopened_server.join().unwrap();
}
