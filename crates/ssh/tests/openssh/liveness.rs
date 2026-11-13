use std::io;
use std::sync::atomic::Ordering;

use super::*;

#[path = "../support/liveness_link.rs"]
mod liveness_link;
use liveness_link::Link;

#[test]
#[ignore = "requires installed OpenSSH ssh-keygen and sshd, and a usable local account"]
fn openssh_liveness_keeps_late_probe_reply_owned_then_detects_a_blackhole() {
    let temp = TempDirectory::new("ssh-liveness");
    let user = fixture_user();
    generate_keys(&temp.0, &["host", "client"]);
    let config = write_config(&temp.0, "host", &user);
    let mut text = fs::read_to_string(&config)
        .unwrap()
        .replace("LogLevel DEBUG2", "LogLevel DEBUG3")
        .replace("AllowTcpForwarding local", "AllowTcpForwarding yes");
    text.push_str(&format!(
        "SetEnv HOME=\"{}\" HISTFILE=/dev/null ENV=/dev/null BASH_ENV=/dev/null\n",
        temp.0.display(),
    ));
    fs::write(&config, text).unwrap();
    let log = temp.0.join("sshd.log");
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let upstream = server.local_addr().unwrap();
    let log_path = log.clone();
    let sshd = thread::spawn(move || {
        let (socket, _) = server.accept().unwrap();
        let input: OwnedFd = socket.into();
        let output = input.try_clone().unwrap();
        Command::new("/usr/sbin/sshd")
            .args(["-ie", "-f"])
            .arg(config)
            .stdin(Stdio::from(input))
            .stdout(Stdio::from(output))
            .stderr(Stdio::from(fs::File::create(log_path).unwrap()))
            .status()
            .unwrap()
    });

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let target = fixture_target(&temp.0, &user, "client", &listener);
    let trust = TrustStore::new(temp.0.join("pins"), TrustMode::Strict);
    pin(&trust, &target.endpoint, &temp.0, "host");
    let link = Arc::new(Link::default());
    let relay = Arc::clone(&link);
    let proxy = thread::spawn(move || relay.run(listener, upstream));
    let mut ssh = SshSession::connect_preconnected_with_options(
        TcpStream::connect(address).unwrap(),
        (target, Vec::new()),
        trust,
        StartupChannel::Terminal {
            term: "xterm",
            size: fixture_size(),
        },
        ConnectOptions::default().with_keepalive(Some(KeepAlive::new(false, 60).unwrap())),
        || false,
    )
    .unwrap();
    let mut input = ssh.take_writer().unwrap();
    let mut output = ssh.take_reader().unwrap();
    let abort = ssh.abort_handle();
    assert_shell_usable(&mut input, &mut output, "before-liveness");
    let bind = "127.0.0.1:0".parse().unwrap();
    let opener = ssh.remote_opener();
    let mut before = opener.bind(bind).unwrap();
    assert_ne!(before.remote_addr().port(), 0);
    before.close().unwrap();

    // Hold the first global failure reply in the owned relay beyond the
    // response window. It must never serve as the next forwarding reply.
    link.hold_inbound.store(true, Ordering::Release);
    let deadline = Instant::now() + Duration::from_secs(6);
    while !fs::read_to_string(&log).is_ok_and(|log| log.contains("rtype keepalive@kortty.de")) {
        assert!(
            Instant::now() < deadline,
            "OpenSSH did not receive the liveness probe"
        );
        thread::sleep(Duration::from_millis(20));
    }
    let failure = match opener.bind(bind) {
        Ok(_) => panic!("a late probe reply must reserve the global slot"),
        Err(error) => error,
    };
    assert!(
        matches!(failure, ConnectError::Io(ref error) if error.kind() == io::ErrorKind::TimedOut && error.to_string().contains("liveness reply is outstanding")),
        "{failure}"
    );
    assert!(!abort.is_closed(), "an unarmed peer stays connected");
    link.hold_inbound.store(false, Ordering::Release);
    assert_shell_usable(&mut input, &mut output, "after-late-liveness");
    let mut after = opener.bind(bind).unwrap();
    assert_ne!(after.remote_addr().port(), 0);
    after.close().unwrap();

    let mut lease = ssh.sftp_opener().unwrap().open().unwrap();
    let mut sftp = Sftp::new(lease.take_reader().unwrap(), lease.take_writer().unwrap()).unwrap();
    assert_eq!(sftp.stat(".").unwrap().kind(), FileKind::Directory);
    drop(sftp);
    drop(lease);

    // Keep both TCP ends open while dropping traffic in both directions.
    // Only the armed SSH probe can detect this as a lost connection.
    let start = Instant::now();
    link.blackhole.store(true, Ordering::Release);
    let deadline = start + Duration::from_secs(11);
    let error = loop {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "liveness deadline was not enforced");
        match output.read_timeout(&mut [0; 1024], left) {
            Err(error) => break error,
            Ok(0) => panic!("the connection ended without its liveness result"),
            Ok(_) => {}
        }
    };
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(error.to_string(), "SSH server stopped responding");
    assert!(start.elapsed() >= Duration::from_secs(5));
    assert!(abort.is_closed());
    ssh.close().unwrap();
    proxy.join().unwrap().unwrap();
    let _ = sshd.join().unwrap();
    let log = fs::read_to_string(log).unwrap();
    assert!(
        log.lines()
            .any(|line| line.ends_with("send packet: type 82")),
        "{log}"
    );
}
